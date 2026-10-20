//! `providers/google/{request,schema,modelId,stream,response}.ts` as seen on
//! the wire of `@google/genai` 2.10.0 (the baseline's pinned SDK), plus the
//! Google-specific parts of `streamModel.ts` (`streamGoogleProviderRequest`,
//! `toGoogleContents`, `resolveGoogleEndpoint`, SDK headers and error bodies).
//!
//! The SDK is not a dependency here: [`build_request`] emits the final
//! `generateContent` JSON body the SDK would POST (the `contents`,
//! `systemInstruction`, `tools`, `toolConfig` and `generationConfig` shape),
//! [`endpoint`] builds `.../models/<id>:streamGenerateContent?alt=sse`, and
//! [`final_body`] applies the SDK's deep `extraBody` merge. Canonical event
//! normalization mirrors `normalizeGoogleStreamEvent`.
use super::super::count_body::{array, field, is, truthy};
use super::super::http::{ModelStreamError, append_js_string};
use super::thinking::ThinkingPlan;
use crate::metadata::js_trim;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::sync::LazyLock;

/// SDK default `x-goog-api-client`/`user-agent` label. The baseline pins
/// `@google/genai` 2.10.0 on Node v22.22.0, and the fixture records that label.
const SDK_HEADER: &str = "google-genai-sdk/2.10.0 gl-node/v22.22.0";

fn units(value: &JsValue) -> Result<JsString, ModelStreamError> {
    crate::js_value::to_string_units_checked(value)
        .map(JsString::from_utf16)
        .map_err(|error| ModelStreamError::Thrown {
            name: error.name().into(),
            message: error.message().into(),
        })
}
fn text(value: &JsValue) -> Result<String, ModelStreamError> {
    Ok(String::from_utf16_lossy(units(value)?.units()))
}
// These TS fields call `.trim()` directly; they do not coerce non-strings.
fn trim_input(value: &JsValue, parameter: &str) -> Result<String, ModelStreamError> {
    if value.as_string().is_some() {
        return text(value);
    }
    let message = match value {
        JsValue::Null => "Cannot read properties of null (reading 'trim')".to_owned(),
        JsValue::Undefined => "Cannot read properties of undefined (reading 'trim')".to_owned(),
        _ => format!("{parameter}.trim is not a function"),
    };
    Err(ModelStreamError::Thrown {
        name: "TypeError".into(),
        message: message.into(),
    })
}
fn value_object() -> JsObject {
    JsObject::new()
}

// ---------------------------------------------------------------------------
// modelId.ts

/// `normalizeGoogleModelId(id)`.
pub(crate) fn normalize_model_id(id: &str) -> String {
    let trimmed = js_trim(id);
    let without_provider = trimmed.strip_prefix("google/").map_or(trimmed, |rest| rest);
    match without_provider {
        "gemini-3-pro" => "gemini-3-pro-preview".into(),
        "gemini-3-flash" => "gemini-3-flash-preview".into(),
        "gemini-3.1-pro" => "gemini-3.1-pro-preview".into(),
        "gemini-3.1-flash-lite" => "gemini-3.1-flash-lite-preview".into(),
        "gemini-3.1-flash" | "gemini-3.1-flash-preview" => "gemini-3-flash-preview".into(),
        other => other.into(),
    }
}

// ---------------------------------------------------------------------------
// request.ts

/// `buildGoogleRequest` projected to the SDK wire body. `Err` is what the SDK
/// path would throw: a canonical `ModelRequestError` for an unsupported
/// thinking plan, or a `TypeError` from `settings.efforts.includes`.
pub(crate) fn build_request(
    request: &JsValue,
    model: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    let tools: Vec<JsValue> = array(field(request, "tools"))
        .iter()
        .map(function_declaration)
        .collect();
    let plan = thinking_plan(request, model)?;
    let thinking_config = thinking_config(plan);

    let mut config = value_object();
    let max_tokens = match field(request, "maxOutputTokens") {
        JsValue::Undefined | JsValue::Null => {
            field(field(model, "capabilities"), "maxOutputTokens").clone()
        }
        value => value.clone(),
    };
    if !max_tokens.is_undefined() {
        config.insert("maxOutputTokens".into(), max_tokens);
    }
    let prompt = field(request, "systemPrompt");
    let system_instruction = truthy(prompt).then(|| {
        let mut content = value_object();
        content.insert("parts".into(), JsValue::Array(vec![text_part(prompt)]));
        content.insert("role".into(), "user".into());
        JsValue::Object(content)
    });
    let tool_config = google_tool_config(field(request, "toolChoice"));
    let output_schema = field(request, "outputSchema");
    if truthy(output_schema) {
        config.insert("responseMimeType".into(), "application/json".into());
        config.insert(
            "responseJsonSchema".into(),
            clean_schema_for_google(field(output_schema, "schema")),
        );
    }
    if let Some(thinking) = thinking_config {
        config.insert("thinkingConfig".into(), thinking);
    }

    let contents = sanitize_google_contents(to_google_contents(request)?);
    let mut body = value_object();
    body.insert("contents".into(), JsValue::Array(contents));
    if let Some(system_instruction) = system_instruction {
        body.insert("systemInstruction".into(), system_instruction);
    }
    if !tools.is_empty() {
        let mut declarations = value_object();
        declarations.insert("functionDeclarations".into(), JsValue::Array(tools));
        body.insert(
            "tools".into(),
            JsValue::Array(vec![JsValue::Object(declarations)]),
        );
    }
    if let Some(tool_config) = tool_config {
        body.insert("toolConfig".into(), tool_config);
    }
    body.insert("generationConfig".into(), JsValue::Object(config));
    // The wire body has no `model`; it lives in the URL path only.
    Ok(JsValue::Object(body))
}

fn text_part(text: &JsValue) -> JsValue {
    let mut part = value_object();
    part.insert("text".into(), text.clone());
    JsValue::Object(part)
}

fn function_declaration(tool: &JsValue) -> JsValue {
    let mut declaration = value_object();
    declaration.insert("name".into(), field(tool, "name").clone());
    declaration.insert("description".into(), field(tool, "description").clone());
    declaration.insert(
        "parametersJsonSchema".into(),
        normalize_google_tool_schema(field(tool, "inputSchema")),
    );
    JsValue::Object(declaration)
}

/// `toGoogleToolConfig(toolChoice)`; mapper key order is
/// `allowedFunctionNames` before `mode`.
fn google_tool_config(choice: &JsValue) -> Option<JsValue> {
    if !truthy(choice) {
        return None;
    }
    let mut mode = value_object();
    for name in ["auto", "none", "required"] {
        if is(choice, name) {
            mode.insert(
                "mode".into(),
                match name {
                    "auto" => "AUTO",
                    "none" => "NONE",
                    _ => "ANY",
                }
                .into(),
            );
            let mut config = value_object();
            config.insert("functionCallingConfig".into(), JsValue::Object(mode));
            return Some(JsValue::Object(config));
        }
    }
    mode.insert(
        "allowedFunctionNames".into(),
        JsValue::Array(vec![field(choice, "name").clone()]),
    );
    mode.insert("mode".into(), "ANY".into());
    let mut config = value_object();
    config.insert("functionCallingConfig".into(), JsValue::Object(mode));
    Some(JsValue::Object(config))
}

/// The baseline resolves Google thinking against a synthetic provider, even
/// when the configured provider has a custom ID. All plan semantics live in
/// the shared registry; only the SDK's canonical error projection is local.
fn thinking_plan(request: &JsValue, model: &JsValue) -> Result<ThinkingPlan, ModelStreamError> {
    let provider =
        super::super::count_body::object([("id", "google".into()), ("protocol", "google".into())]);
    let plan = super::thinking::resolve(request, &provider, model)?;
    if let Err(ModelStreamError::Request { message, .. }) =
        super::thinking::throw_if_unsupported(&plan)
    {
        let mode = plan.mode.clone().unwrap_or_else(|| "default".into());
        return Err(ModelStreamError::Provider(thinking_error(
            request, &mode, message,
        )));
    }
    Ok(plan)
}

fn thinking_config(plan: ThinkingPlan) -> Option<JsValue> {
    if !plan.enabled {
        return None;
    }
    let mut config = value_object();
    config.insert("includeThoughts".into(), JsValue::Bool(true));
    if let Some(level) = plan.thinking_level {
        config.insert("thinkingLevel".into(), JsValue::String(level));
    }
    Some(JsValue::Object(config))
}

/// `throwIfUnsupportedThinkingPlan` + `ModelRequestError` projection.
fn thinking_error(request: &JsValue, mode: &JsString, message: JsString) -> JsObject {
    let mut details = value_object();
    details.insert("provider".into(), field(request, "provider").clone());
    details.insert("model".into(), field(request, "model").clone());
    details.insert("thinkingMode".into(), JsValue::String(mode.clone()));
    let mut raw = value_object();
    raw.insert("code".into(), "unsupported_thinking".into());
    raw.insert("details".into(), JsValue::Object(details));
    raw.insert("name".into(), "ModelRequestError".into());
    let mut canonical = value_object();
    canonical.insert("provider".into(), field(request, "provider").clone());
    canonical.insert("protocol".into(), "google".into());
    canonical.insert("code".into(), "unsupported_thinking".into());
    canonical.insert("message".into(), JsValue::String(message));
    canonical.insert("retryable".into(), JsValue::Bool(false));
    canonical.insert("raw".into(), JsValue::Object(raw));
    canonical
}

/// `toGoogleContents`: group consecutive blocks into `user`/`model` contents.
fn to_google_contents(request: &JsValue) -> Result<Vec<(String, Vec<JsValue>)>, ModelStreamError> {
    let mut tool_names: Vec<(JsString, JsValue)> = Vec::new();
    for message in array(field(request, "messages")) {
        for block in array(field(message, "content")) {
            if is(field(block, "type"), "tool_call") {
                let id = JsString::from(sanitize_tool_call_id(
                    &trim_input(field(block, "id"), "id")?,
                    "call",
                ));
                let name = field(block, "name").clone();
                set_name_pair(&mut tool_names, id, name);
            }
        }
    }
    let mut contents: Vec<(String, Vec<JsValue>)> = Vec::new();
    for message in array(field(request, "messages")) {
        let mut current_role: Option<String> = None;
        let mut current_parts: Vec<JsValue> = Vec::new();
        let mut flush = |role: &mut Option<String>, parts: &mut Vec<JsValue>| {
            if let Some(role) = role.take()
                && !parts.is_empty()
            {
                contents.push((role, std::mem::take(parts)));
            }
            parts.clear();
        };
        for block in array(field(message, "content")) {
            let tag = field(block, "type");
            let role = if is(tag, "tool_result") || is(tag, "tool_result_reference") {
                "user"
            } else if is(field(message, "role"), "assistant") {
                "model"
            } else {
                "user"
            };
            let parts = to_google_parts(block, &tool_names)?;
            if parts.is_empty() {
                continue;
            }
            if current_role.as_deref() != Some(role) {
                flush(&mut current_role, &mut current_parts);
                current_role = Some(role.to_string());
            }
            current_parts.extend(parts);
        }
        flush(&mut current_role, &mut current_parts);
    }
    Ok(contents)
}

fn set_name_pair(pairs: &mut Vec<(JsString, JsValue)>, key: JsString, value: JsValue) {
    if let Some(slot) = pairs.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = value;
    } else {
        pairs.push((key, value));
    }
}
fn get_name_pair<'a>(pairs: &'a [(JsString, JsValue)], key: &JsString) -> Option<&'a JsValue> {
    pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}
fn to_google_parts(
    block: &JsValue,
    tool_names: &[(JsString, JsValue)],
) -> Result<Vec<JsValue>, ModelStreamError> {
    let tag = field(block, "type");
    if is(tag, "text") {
        let value = field(block, "text");
        if value.as_string().is_some_and(|s| !s.is_empty()) {
            return Ok(vec![text_part(value)]);
        }
        return Ok(Vec::new());
    }
    if is(tag, "thinking") {
        let mut part = value_object();
        part.insert("text".into(), field(block, "text").clone());
        part.insert("thought".into(), JsValue::Bool(true));
        let signature = field(block, "signature");
        if truthy(signature) {
            part.insert("thoughtSignature".into(), signature.clone());
        }
        return Ok(vec![JsValue::Object(part)]);
    }
    if is(tag, "image") || is(tag, "pdf") || is(tag, "audio") {
        return Ok(vec![media_part(block)]);
    }
    if is(tag, "tool_call") {
        let mut function_call = value_object();
        function_call.insert(
            "id".into(),
            sanitize_tool_call_id(&trim_input(field(block, "id"), "id")?, "call")
                .as_str()
                .into(),
        );
        function_call.insert("args".into(), google_args(field(block, "input")));
        function_call.insert("name".into(), field(block, "name").clone());
        let mut part = value_object();
        part.insert("functionCall".into(), JsValue::Object(function_call));
        return Ok(vec![JsValue::Object(part)]);
    }
    if is(tag, "tool_result") {
        let id = sanitize_tool_call_id(&trim_input(field(block, "toolCallId"), "id")?, "call");
        let name = get_name_pair(tool_names, &JsString::from(id.as_str()))
            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            .cloned()
            .unwrap_or_else(|| field(block, "toolCallId").clone());
        // Shared UTF-16 helper from the compaction layer (same TS function);
        // keeps lone surrogates intact for JSON.stringify.
        let output = JsValue::String(
            crate::runtime::context::compaction::flatten_tool_result_block_text(block),
        );
        let content = array(field(block, "content"));
        return Ok(vec![function_response_part(
            &id,
            &name,
            &output,
            truthy(field(block, "isError")),
            content,
        )]);
    }
    if is(tag, "tool_result_reference") {
        let id = sanitize_tool_call_id(&trim_input(field(block, "toolCallId"), "id")?, "call");
        let name = get_name_pair(tool_names, &JsString::from(id.as_str()))
            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            .cloned()
            .unwrap_or_else(|| field(block, "toolCallId").clone());
        // Shared with the anthropic/count-body path; `count_body` keeps the
        // value as a `JsString`, so lone surrogates survive.
        let output = super::super::count_body::reference_text(block);
        return Ok(vec![function_response_part(
            &id,
            &name,
            &output,
            truthy(field(block, "isError")),
            &[],
        )]);
    }
    if is(tag, "media_reference") {
        return Ok(vec![text_part(field(block, "preview"))]);
    }
    Ok(Vec::new())
}

fn media_part(block: &JsValue) -> JsValue {
    let mut inner = value_object();
    let mut part = value_object();
    if is(field(block, "source"), "url") {
        inner.insert("fileUri".into(), field(block, "data").clone());
        inner.insert("mimeType".into(), field(block, "mimeType").clone());
        part.insert("fileData".into(), JsValue::Object(inner));
    } else {
        inner.insert("data".into(), field(block, "data").clone());
        inner.insert("mimeType".into(), field(block, "mimeType").clone());
        part.insert("inlineData".into(), JsValue::Object(inner));
    }
    JsValue::Object(part)
}

fn function_response_part(
    id: &str,
    name: &JsValue,
    output: &JsValue,
    is_error: bool,
    content: &[JsValue],
) -> JsValue {
    let mut response = value_object();
    if is_error {
        response.insert(
            "error".into(),
            if truthy(output) {
                output.clone()
            } else {
                "Tool execution failed.".into()
            },
        );
    } else {
        response.insert("output".into(), output.clone());
    }
    let parts: Vec<JsValue> = content.iter().flat_map(media_response_part).collect();
    let mut inner = value_object();
    inner.insert("id".into(), id.into());
    inner.insert("name".into(), name.clone());
    inner.insert("response".into(), JsValue::Object(response));
    if !parts.is_empty() {
        inner.insert("parts".into(), JsValue::Array(parts));
    }
    let mut part = value_object();
    part.insert("functionResponse".into(), JsValue::Object(inner));
    JsValue::Object(part)
}

fn media_response_part(block: &JsValue) -> Vec<JsValue> {
    if is(field(block, "type"), "text") {
        return Vec::new();
    }
    vec![media_part(block)]
}

/// `toGoogleArgs(input)`; mapper order is `id`, `args`, `name`.
fn google_args(input: &JsValue) -> JsValue {
    let mut args = value_object();
    if matches!(input, JsValue::Object(_)) {
        args = input.as_object().cloned().unwrap_or_default();
    } else {
        args.insert("value".into(), input.clone());
    }
    JsValue::Object(args)
}

fn sanitize_google_contents(contents: Vec<(String, Vec<JsValue>)>) -> Vec<JsValue> {
    let mut merged: Vec<(String, Vec<JsValue>)> = Vec::new();
    for (role, parts) in contents {
        if parts.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some((last_role, last_parts)) if *last_role == role => {
                last_parts.extend(parts);
            }
            _ => merged.push((role, parts)),
        }
    }
    if merged.first().is_some_and(|(role, _)| role == "model") {
        merged.insert(
            0,
            (
                "user".into(),
                vec![text_part(&JsValue::from(
                    "Continue the conversation from the available context.",
                ))],
            ),
        );
    }
    if merged.is_empty() {
        merged.push(("user".into(), vec![text_part(&JsValue::from(""))]));
    }
    merged
        .into_iter()
        .map(|(role, parts)| {
            let mut content = value_object();
            content.insert("parts".into(), JsValue::Array(parts));
            content.insert("role".into(), role.as_str().into());
            JsValue::Object(content)
        })
        .collect()
}

/// `sanitizeGoogleToolCallId` (`fallback` is `"call"` in requests and
/// `"google"` for response ids).
fn sanitize_tool_call_id(id: &str, fallback: &str) -> String {
    let trimmed = js_trim(id);
    let mut out = String::new();
    let mut pending_underscore = false;
    for c in trimmed.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            if pending_underscore && !out.is_empty() {
                out.push('_');
            }
            pending_underscore = false;
            out.push(c);
        } else {
            pending_underscore = true;
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        fallback.into()
    } else {
        out.into()
    }
}

// ---------------------------------------------------------------------------
// schema.ts

const UNSUPPORTED_SCHEMA_KEYWORDS: [&str; 20] = [
    "patternProperties",
    "additionalProperties",
    "$schema",
    "$id",
    "$ref",
    "$defs",
    "definitions",
    "examples",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "multipleOf",
    "pattern",
    "format",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];
const META_KEYS: [&str; 3] = ["description", "title", "default"];

fn is_record(value: &JsValue) -> bool {
    value.as_object().is_some()
}
static LOCAL_REF: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^#/(?:\$defs|definitions)/(.+)$").expect("static regex"));
static VALID_PARAMETER_NAME: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$").expect("static regex"));

fn copy_schema_meta(from: &JsObject, to: &mut JsObject) {
    for key in META_KEYS {
        let source = from.get(key);
        let target = to.get(key);
        if target.is_none_or(JsValue::is_undefined)
            && let Some(source) = source
            && !source.is_undefined()
        {
            to.insert(key.into(), source.clone());
        }
    }
}

// Assignment to `{}` invokes Object.prototype.__proto__; object spread instead
// defines own data properties. Keep that distinction local to the TS/SDK walks.
fn assign_object(target: &mut JsObject, key: JsString, value: JsValue) {
    if key.equals_str("__proto__") {
        let mut cursor = &*target;
        loop {
            if cursor.contains_key("__proto__") {
                break;
            }
            match cursor.prototype() {
                None | Some(JsValue::Array(_)) => {
                    if matches!(
                        value,
                        JsValue::Object(_) | JsValue::Array(_) | JsValue::Null
                    ) {
                        target.set_prototype(value);
                    }
                    return;
                }
                Some(JsValue::Object(parent)) => cursor = parent,
                Some(_) => break,
            }
        }
    }
    target.insert(key, value);
}

// Move own entries, without retaining a prototype or cloning each deep subtree.
fn copy_own_object(mut source: JsObject, assignment: bool) -> JsObject {
    let keys: Vec<_> = source
        .own_entries()
        .into_iter()
        .map(|(key, _)| key.clone())
        .collect();
    let mut result = value_object();
    for key in keys {
        let value = source
            .insert(key.clone(), JsValue::Undefined)
            .expect("own key");
        if assignment {
            assign_object(&mut result, key, value);
        } else {
            result.insert(key, value);
        }
    }
    result
}

fn property_value(object: &JsObject, key: &JsString) -> Option<JsValue> {
    use crate::config::changes::{Operand, object_property};
    object_property(object, key).map(|value| match value {
        Operand::Value(value) => value.clone(),
        Operand::Number(value) => JsValue::Number(value),
        Operand::ObjectPrototype => {
            let mut prototype = value_object();
            prototype.set_prototype(JsValue::Null);
            JsValue::Object(prototype)
        }
        // Native functions have no schema fields and serialize as undefined.
        Operand::Function(_) => JsValue::Undefined,
    })
}

/// Heap-backed traversal: properties, local refs and single-branch unions all
/// schedule work instead of consuming the caller's (normally 2 MiB) stack.
fn clean_schema(schema: &JsValue) -> JsValue {
    use std::rc::Rc;
    type Defs<'a> = Rc<Vec<(JsString, &'a JsValue)>>;
    type Refs = Rc<Vec<String>>;
    enum Work<'a> {
        Visit(&'a JsValue, Defs<'a>, Refs),
        Properties(&'a JsValue, Defs<'a>, Refs),
        Value(JsValue),
        Array(usize),
        Object(Vec<JsString>, Option<&'a JsObject>),
        Meta(&'a JsObject),
    }
    let mut work = vec![Work::Visit(schema, Rc::default(), Rc::default())];
    let mut values = Vec::new();
    while let Some(task) = work.pop() {
        match task {
            Work::Value(value) => values.push(value),
            Work::Array(count) => {
                let items = values.split_off(values.len() - count);
                values.push(JsValue::Array(items));
            }
            Work::Object(keys, source) => {
                let children = values.split_off(values.len() - keys.len());
                let mut result = value_object();
                for (key, value) in keys.into_iter().zip(children) {
                    assign_object(&mut result, key, value);
                }
                if let Some(source) = source {
                    if result
                        .get_property("type")
                        .is_some_and(|value| is(value, "array"))
                        && result
                            .get_property("items")
                            .is_none_or(JsValue::is_undefined)
                    {
                        result.insert("items".into(), JsValue::Object(value_object()));
                    }
                    let required =
                        clean_required(source.get("required"), result.get_property("properties"));
                    if !required.is_empty() {
                        result.insert(
                            "required".into(),
                            JsValue::Array(required.into_iter().map(JsValue::String).collect()),
                        );
                    }
                }
                values.push(JsValue::Object(result));
            }
            Work::Meta(source) => {
                if let Some(JsValue::Object(result)) = values.last_mut() {
                    *result = copy_own_object(std::mem::take(result), false);
                    copy_schema_meta(source, result);
                }
            }
            Work::Properties(value, defs, refs) => {
                let entries = value
                    .as_object()
                    .map(JsObject::own_entries)
                    .unwrap_or_default();
                let entries: Vec<_> = entries
                    .into_iter()
                    .filter(|(key, _)| {
                        VALID_PARAMETER_NAME.is_match(&String::from_utf16_lossy(key.units()))
                    })
                    .collect();
                work.push(Work::Object(
                    entries.iter().map(|(key, _)| (*key).clone()).collect(),
                    None,
                ));
                work.extend(
                    entries
                        .into_iter()
                        .rev()
                        .map(|(_, value)| Work::Visit(value, defs.clone(), refs.clone())),
                );
            }
            Work::Visit(value, defs, refs) => match value {
                JsValue::Array(items) => {
                    work.push(Work::Array(items.len()));
                    work.extend(
                        items
                            .iter()
                            .rev()
                            .map(|item| Work::Visit(item, defs.clone(), refs.clone())),
                    );
                }
                JsValue::Object(obj) => {
                    let mut defs = defs;
                    for entry in [obj.get("$defs"), obj.get("definitions")]
                        .into_iter()
                        .flatten()
                        .filter_map(JsValue::as_object)
                    {
                        let next = Rc::make_mut(&mut defs);
                        for (key, value) in entry.own_entries() {
                            if let Some((_, existing)) = next.iter_mut().find(|(k, _)| k == key) {
                                *existing = value;
                            } else {
                                next.push((key.clone(), value));
                            }
                        }
                    }
                    if let Some(reference) = obj
                        .get("$ref")
                        .and_then(JsValue::as_string)
                        .filter(|s| !s.is_empty())
                    {
                        let reference = String::from_utf16_lossy(reference.units());
                        if refs.contains(&reference) {
                            values.push(JsValue::Object(value_object()));
                            continue;
                        }
                        let resolved = LOCAL_REF
                            .captures(&reference)
                            .and_then(|capture| {
                                let key = JsString::from(
                                    capture[1].replace("~1", "/").replace("~0", "~"),
                                );
                                defs.iter()
                                    .find(|(name, _)| name == &key)
                                    .map(|(_, value)| *value)
                            })
                            .filter(|value| !value.is_undefined());
                        work.push(Work::Meta(obj));
                        if let Some(resolved) = resolved {
                            let mut refs = refs;
                            Rc::make_mut(&mut refs).push(reference);
                            work.push(Work::Visit(resolved, defs, refs));
                        } else {
                            work.push(Work::Value(JsValue::Object(value_object())));
                        }
                        continue;
                    }
                    let variants = match obj.get("anyOf") {
                        Some(JsValue::Array(items)) => Some(items),
                        _ => match obj.get("oneOf") {
                            Some(JsValue::Array(items)) => Some(items),
                            _ => None,
                        },
                    };
                    if let Some(variants) = variants {
                        let non_null: Vec<_> =
                            variants.iter().filter(|v| !is_null_schema(v)).collect();
                        if let Some(mut union) = try_flatten_literal_union(&non_null) {
                            copy_schema_meta(obj, &mut union);
                            values.push(JsValue::Object(union));
                            continue;
                        }
                        if non_null.len() == 1 {
                            work.push(Work::Meta(obj));
                            work.push(Work::Visit(non_null[0], defs, refs));
                            continue;
                        }
                    }
                    let mut keys = Vec::new();
                    let mut children = Vec::new();
                    for (key, value) in obj.own_entries() {
                        let name = String::from_utf16_lossy(key.units());
                        if UNSUPPORTED_SCHEMA_KEYWORDS.contains(&name.as_ref())
                            || name == "required"
                        {
                            continue;
                        }
                        keys.push(if name == "const" {
                            "enum".into()
                        } else {
                            key.clone()
                        });
                        children.push(match name.as_ref() {
                            "const" => Work::Value(JsValue::Array(vec![value.clone()])),
                            "type" => Work::Value(normalize_type(value)),
                            "properties" => Work::Properties(value, defs.clone(), refs.clone()),
                            _ => Work::Visit(value, defs.clone(), refs.clone()),
                        });
                    }
                    work.push(Work::Object(keys, Some(obj)));
                    work.extend(children.into_iter().rev());
                }
                other => values.push(other.clone()),
            },
        }
    }
    values.pop().expect("one cleaned schema")
}

fn clean_required(required: Option<&JsValue>, properties: Option<&JsValue>) -> Vec<JsString> {
    let (Some(JsValue::Array(required)), Some(properties)) = (required, properties) else {
        return Vec::new();
    };
    let Some(properties) = properties.as_object() else {
        return Vec::new();
    };
    required
        .iter()
        .filter_map(JsValue::as_string)
        .filter(|key| crate::config::changes::object_property(properties, key).is_some())
        .cloned()
        .collect()
}

fn normalize_type(value: &JsValue) -> JsValue {
    let JsValue::Array(items) = value else {
        if is(value, "null") {
            return JsValue::Undefined;
        }
        return value.clone();
    };
    let without_null: Vec<JsValue> = items
        .iter()
        .filter(|item| !is(item, "null"))
        .cloned()
        .collect();
    if without_null.len() == 1 {
        without_null.into_iter().next().expect("one")
    } else {
        JsValue::Array(without_null)
    }
}

fn is_null_schema(value: &JsValue) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    if record.get("type").is_some_and(|value| is(value, "null"))
        || record.get("const") == Some(&JsValue::Null)
    {
        return true;
    }
    matches!(
        record.get("enum"),
        Some(JsValue::Array(items)) if items.len() == 1 && items[0] == JsValue::Null
    )
}

fn try_flatten_literal_union(variants: &[&JsValue]) -> Option<JsObject> {
    if variants.is_empty() {
        return None;
    }
    let mut values = Vec::new();
    let mut common: Option<String> = None;
    for variant in variants {
        let (kind, value) = extract_single_enum_value(variant)?;
        match &common {
            Some(existing) if *existing != kind => return None,
            Some(_) => {}
            None => common = Some(kind),
        }
        values.push(value);
    }
    let kind = common?;
    let mut out = value_object();
    out.insert("type".into(), kind.as_str().into());
    out.insert("enum".into(), JsValue::Array(values));
    Some(out)
}

fn type_of(value: &JsValue) -> &'static str {
    match value {
        JsValue::Undefined => "undefined",
        JsValue::Bool(_) => "boolean",
        JsValue::Number(_) => "number",
        JsValue::String(_) => "string",
        JsValue::Null | JsValue::Array(_) | JsValue::Object(_) => "object",
    }
}

fn extract_single_enum_value(schema: &JsValue) -> Option<(String, JsValue)> {
    let record = schema.as_object()?;
    let value = match record.get("const") {
        Some(value) => value.clone(),
        None => match record.get("enum") {
            Some(JsValue::Array(items)) if items.len() == 1 => items[0].clone(),
            _ => JsValue::Undefined,
        },
    };
    let kind = match record.get("type") {
        Some(JsValue::String(kind)) => String::from_utf16_lossy(kind.units()),
        _ => type_of(&value).to_string(),
    };
    if value.is_undefined() || kind == "undefined" {
        return None;
    }
    Some((kind, value))
}

fn extract_enum_values(schema: &JsValue) -> Option<Vec<JsValue>> {
    let record = schema.as_object()?;
    // A direct empty enum is Some([]), unlike an empty union traversal.
    if let Some(JsValue::Array(items)) = record.get("enum") {
        return Some(items.clone());
    }
    let mut pending = vec![schema];
    let mut values = Vec::new();
    while let Some(schema) = pending.pop() {
        let Some(record) = schema.as_object() else {
            continue;
        };
        if let Some(JsValue::Array(items)) = record.get("enum") {
            values.extend(items.iter().cloned());
        } else if let Some(value) = record.get("const") {
            values.push(value.clone());
        } else {
            let variants = match record.get("anyOf") {
                Some(JsValue::Array(items)) => Some(items),
                _ => match record.get("oneOf") {
                    Some(JsValue::Array(items)) => Some(items),
                    _ => None,
                },
            };
            if let Some(variants) = variants {
                pending.extend(variants.iter().rev());
            }
        }
    }
    (!values.is_empty()).then_some(values)
}

fn same_value_zero(a: &JsValue, b: &JsValue) -> bool {
    match (a, b) {
        (JsValue::Number(a), JsValue::Number(b)) => (a.is_nan() && b.is_nan()) || a == b,
        (JsValue::String(a), JsValue::String(b)) => a == b,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::Null, JsValue::Null) | (JsValue::Undefined, JsValue::Undefined) => true,
        // Object identity is unobservable for schemas built from JSON: two
        // structural copies are never the same reference, so never dedupe.
        (JsValue::Array(_), JsValue::Array(_)) | (JsValue::Object(_), JsValue::Object(_)) => false,
        _ => false,
    }
}

fn merge_property_schemas(existing: &JsValue, incoming: &JsValue) -> JsValue {
    let existing_values = extract_enum_values(existing);
    let incoming_values = extract_enum_values(incoming);
    if existing_values.is_some() || incoming_values.is_some() {
        let mut values: Vec<JsValue> = Vec::new();
        for value in existing_values
            .unwrap_or_default()
            .into_iter()
            .chain(incoming_values.unwrap_or_default())
        {
            if !values.iter().any(|seen| same_value_zero(seen, &value)) {
                values.push(value);
            }
        }
        let kind = values
            .first()
            .map(type_of)
            .filter(|kind| values.iter().all(|value| type_of(value) == *kind));
        let mut result = value_object();
        result.insert("enum".into(), JsValue::Array(values));
        if let Some(kind) = kind {
            result.insert("type".into(), kind.into());
        }
        for source in [existing, incoming] {
            if let Some(source) = source.as_object() {
                copy_schema_meta(source, &mut result);
            }
        }
        return JsValue::Object(result);
    }
    if existing.is_undefined() || matches!(existing, JsValue::Null) {
        incoming.clone()
    } else {
        existing.clone()
    }
}

/// `flattenTopLevelObjectUnion`; non-record schemas are returned as-is.
fn flatten_top_level_object_union(schema: &JsValue) -> JsValue {
    let Some(obj) = schema.as_object() else {
        return schema.clone();
    };
    let variant_key = match obj.get("anyOf") {
        Some(JsValue::Array(_)) => "anyOf",
        _ => match obj.get("oneOf") {
            Some(JsValue::Array(_)) => "oneOf",
            _ => return schema.clone(),
        },
    };
    let variants: Vec<&JsObject> = match obj.get(variant_key) {
        Some(JsValue::Array(items)) => items
            .iter()
            .filter_map(JsValue::as_object)
            .filter(|variant| variant.get("properties").is_some_and(is_record))
            .collect(),
        _ => Vec::new(),
    };
    if variants.is_empty() {
        return schema.clone();
    }
    let mut merged = value_object();
    let mut function_keys = Vec::new();
    let mut required_counts: Vec<(JsString, usize)> = Vec::new();
    for variant in &variants {
        if let Some(properties) = variant.get("properties").and_then(JsValue::as_object) {
            for (key, value) in properties.own_entries() {
                let function = function_keys.contains(key)
                    || matches!(
                        crate::config::changes::object_property(&merged, key),
                        Some(crate::config::changes::Operand::Function(_))
                    );
                let value = if function && extract_enum_values(value).is_none() {
                    // Retain a native function across later variants. Its wire
                    // representation is undefined, but `existing ?? incoming`
                    // must not treat that representation as a missing value.
                    if !function_keys.contains(key) {
                        function_keys.push(key.clone());
                    }
                    JsValue::Undefined
                } else {
                    function_keys.retain(|name| name != key);
                    match property_value(&merged, key) {
                        Some(existing) => merge_property_schemas(&existing, value),
                        None => value.clone(),
                    }
                };
                assign_object(&mut merged, key.clone(), value);
            }
        }
        if let Some(JsValue::Array(required)) = variant.get("required") {
            for key in required {
                if let Some(key) = key.as_string() {
                    match required_counts.iter_mut().find(|(k, _)| k == key) {
                        Some((_, count)) => *count += 1,
                        None => required_counts.push((key.clone(), 1)),
                    }
                }
            }
        }
    }
    let base_required: Option<Vec<JsString>> = match obj.get("required") {
        Some(JsValue::Array(items)) => Some(
            items
                .iter()
                .filter_map(JsValue::as_string)
                .cloned()
                .collect(),
        ),
        _ => None,
    };
    let required: Vec<JsString> = match base_required {
        Some(values) if !values.is_empty() => values,
        _ => required_counts
            .into_iter()
            .filter(|(_, count)| *count == variants.len())
            .map(|(key, _)| key)
            .collect(),
    };
    let mut base = value_object();
    for (key, value) in obj.own_entries() {
        if key.equals_str("anyOf") || key.equals_str("oneOf") || key.equals_str("required") {
            continue;
        }
        base.insert(key.clone(), value.clone());
    }
    base.insert("type".into(), "object".into());
    base.insert("properties".into(), JsValue::Object(merged));
    if !required.is_empty() {
        base.insert(
            "required".into(),
            JsValue::Array(required.into_iter().map(JsValue::String).collect()),
        );
    }
    JsValue::Object(base)
}

/// `normalizeGoogleToolSchema`.
pub(crate) fn normalize_google_tool_schema(schema: &JsValue) -> JsValue {
    let flattened = flatten_top_level_object_union(schema);
    let cleaned = clean_schema_for_google(&flattened);
    match cleaned.as_object() {
        Some(cleaned) => {
            let mut result = copy_own_object(cleaned.clone(), false);
            if result.get("type").is_none_or(JsValue::is_undefined) {
                result.insert("type".into(), "object".into());
            }
            if !result.get("properties").is_some_and(is_record) {
                result.insert("properties".into(), JsValue::Object(value_object()));
            }
            JsValue::Object(result)
        }
        None => {
            let mut result = value_object();
            result.insert("type".into(), "object".into());
            result.insert("properties".into(), JsValue::Object(value_object()));
            JsValue::Object(result)
        }
    }
}

/// `cleanSchemaForGoogle`.
pub(crate) fn clean_schema_for_google(schema: &JsValue) -> JsValue {
    clean_schema(schema)
}

// ---------------------------------------------------------------------------
// client.ts / SDK transport

/// `resolveGoogleEndpoint(rawUrl)` -> `(baseUrl, apiVersion)`.
fn resolve_endpoint(raw_url: &str) -> (String, String) {
    let trimmed = js_trim(raw_url);
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() {
        return (
            "https://generativelanguage.googleapis.com/".into(),
            "v1beta".into(),
        );
    }
    let Ok(mut url) = url::Url::parse(trimmed) else {
        return (trimmed.to_string(), String::new());
    };
    let segments: Vec<String> = url
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect();
    let last = segments.last().cloned();
    if matches!(last.as_deref(), Some("v1" | "v1beta")) {
        let path = if segments.len() > 1 {
            format!("/{}/", segments[..segments.len() - 1].join("/"))
        } else {
            "/".into()
        };
        url.set_path(&path);
        url.set_query(None);
        url.set_fragment(None);
        return (url.to_string(), last.expect("version segment").to_string());
    }
    if url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("generativelanguage.googleapis.com"))
        && segments.is_empty()
    {
        return (
            "https://generativelanguage.googleapis.com/".into(),
            "v1beta".into(),
        );
    }
    let path = if url.path().ends_with('/') {
        url.path().to_string()
    } else {
        format!("{}/", url.path())
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    (url.to_string(), String::new())
}

/// `createGoogleClient(...).models.generateContentStream` endpoint, without
/// the leading model path rewriting performed by `tModel`.
pub(crate) fn endpoint(provider: &JsValue, model: &JsValue) -> Result<String, ModelStreamError> {
    let (base_url, api_version) = resolve_endpoint(&trim_input(field(provider, "url"), "rawUrl")?);
    let base = base_url.trim_end_matches('/');
    let mut url = String::new();
    url.push_str(base);
    if !api_version.is_empty() {
        url.push('/');
        url.push_str(&api_version);
    }
    let model = normalize_model_id(&trim_input(model, "id")?);
    let path = if model.starts_with("models/") || model.starts_with("tunedModels/") {
        model
    } else {
        format!("models/{model}")
    };
    url.push('/');
    url.push_str(&path);
    url.push_str(":streamGenerateContent?alt=sse");
    Ok(url)
}

/// SDK request headers. `user-agent` is transport-level (excluded by fixtures);
/// `x-goog-api-client` is the SDK's own protocol header.
pub(crate) fn headers(provider: &JsValue) -> Result<Vec<(JsString, JsValue)>, ModelStreamError> {
    let mut headers = value_object();
    headers.insert("content-type".into(), "application/json".into());
    if let Some(custom) = field(provider, "headers").as_object() {
        for (name, value) in custom.own_entries() {
            headers.insert(name.clone(), value.clone());
        }
    }
    let has_client = headers.own_entries().iter().any(|(name, _)| {
        String::from_utf16_lossy(name.units()).eq_ignore_ascii_case("x-goog-api-client")
    });
    if !has_client {
        headers.insert("x-goog-api-client".into(), SDK_HEADER.into());
    }
    let has_key = headers.own_entries().iter().any(|(name, _)| {
        String::from_utf16_lossy(name.units()).eq_ignore_ascii_case("x-goog-api-key")
    });
    if !has_key {
        let key = text(field(provider, "apiKey"))?;
        headers.insert("x-goog-api-key".into(), js_trim(&key).into());
    }
    Ok(headers
        .own_entries()
        .into_iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect())
}

/// `sanitizeProviderBody` + the SDK's recursive `extraBody` merge.
pub(crate) fn final_body(body: &JsValue, provider: &JsValue) -> JsValue {
    let mut sanitized = value_object();
    if let Some(extra) = field(provider, "extraBody").as_object() {
        for (key, value) in extra.own_entries() {
            if key.equals_str("temperature") {
                continue;
            }
            if key.equals_str("generationConfig") {
                let mut config = value_object();
                if let Some(config_in) = value.as_object() {
                    for (inner_key, inner_value) in config_in.own_entries() {
                        if inner_key.equals_str("temperature") {
                            continue;
                        }
                        config.insert(inner_key.clone(), inner_value.clone());
                    }
                    sanitized.insert(key.clone(), JsValue::Object(config));
                    continue;
                }
            }
            sanitized.insert(key.clone(), value.clone());
        }
    }
    // SDK patchHttpOptions shallow-copies extraBody with Object.assign before
    // deepMerge visits its own keys. A root __proto__ is consumed at that step;
    // untouched nested payloads still retain their own __proto__ property.
    let sanitized = copy_own_object(sanitized, true);
    if sanitized.is_empty() {
        return body.clone();
    }
    let merged = deep_merge(body.as_object().cloned().unwrap_or_default(), &sanitized);
    JsValue::Object(merged)
}

fn deep_merge(target: JsObject, source: &JsObject) -> JsObject {
    struct Frame<'a> {
        output: JsObject,
        entries: std::vec::IntoIter<(&'a JsString, &'a JsValue)>,
        parent_key: Option<JsString>,
    }
    let mut stack = vec![Frame {
        output: copy_own_object(target, true),
        entries: source.own_entries().into_iter(),
        parent_key: None,
    }];
    loop {
        let frame = stack.last_mut().expect("root merge frame");
        if let Some((key, source)) = frame.entries.next() {
            let mut target = if matches!(frame.output.get_key(key), Some(JsValue::Object(_))) {
                // Preserve an own key's insertion slot, moving its deep subtree.
                frame.output.insert(key.clone(), JsValue::Undefined)
            } else {
                property_value(&frame.output, key)
            };
            if let (Some(JsValue::Object(target)), JsValue::Object(source)) = (&mut target, source)
            {
                // Move an owned target; inherited targets must be cloned so the
                // prototype remains unchanged when the merged own key is added.
                stack.push(Frame {
                    output: copy_own_object(std::mem::take(target), true),
                    entries: source.own_entries().into_iter(),
                    parent_key: Some(key.clone()),
                });
            } else {
                assign_object(&mut frame.output, key.clone(), source.clone());
            }
        } else {
            let finished = stack.pop().expect("completed frame");
            if let Some(key) = finished.parent_key {
                assign_object(
                    &mut stack.last_mut().expect("parent").output,
                    key,
                    JsValue::Object(finished.output),
                );
            } else {
                return finished.output;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// response.ts (usage / finish reason)

/// `normalizeGoogleFinishReason`.
pub(crate) fn normalize_finish_reason(reason: &JsValue) -> &'static str {
    let reason = match reason {
        JsValue::String(value) => String::from_utf16_lossy(value.units()).to_uppercase(),
        _ => return "unknown",
    };
    match reason.as_str() {
        "STOP" => "stop",
        "MAX_TOKENS" => "length",
        "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" => "tool_call",
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY" => {
            "content_filter"
        }
        _ => "unknown",
    }
}

fn read_string(value: &JsValue) -> Option<&JsString> {
    value.as_string()
}
fn read_number(value: &JsValue) -> Option<f64> {
    value.as_number().filter(|value| value.is_finite())
}
fn sum_defined(values: &[Option<f64>]) -> Option<f64> {
    let mut any = false;
    let mut total = 0.0;
    for value in values.iter().flatten() {
        any = true;
        total += value;
    }
    any.then_some(total)
}

/// `normalizeGoogleUsage`. This is a protocol-specific projection, unlike the
/// OpenAI/Responses shared usage shape: output includes thoughts, input is
/// clamped at zero, and no cache-write/native-cost fields are exposed.
pub(crate) fn normalize_usage(raw: &JsValue) -> Option<JsValue> {
    raw.as_object()?;
    let prompt = read_number(field(raw, "promptTokenCount"));
    let response = read_number(field(raw, "candidatesTokenCount"))
        .or_else(|| read_number(field(raw, "responseTokenCount")));
    let thinking = read_number(field(raw, "thoughtsTokenCount"));
    let output = sum_defined(&[response, thinking]);
    let cache_read = read_number(field(raw, "cachedContentTokenCount"));
    let input = prompt.map(|prompt| (prompt - cache_read.unwrap_or(0.0)).max(0.0));
    let total =
        read_number(field(raw, "totalTokenCount")).or_else(|| sum_defined(&[prompt, output]));
    let mut usage = value_object();
    usage.insert(
        "inputTokens".into(),
        input.map_or(JsValue::Undefined, JsValue::Number),
    );
    usage.insert(
        "outputTokens".into(),
        output.map_or(JsValue::Undefined, JsValue::Number),
    );
    usage.insert(
        "cacheReadTokens".into(),
        cache_read.map_or(JsValue::Undefined, JsValue::Number),
    );
    usage.insert(
        "totalTokens".into(),
        total.map_or(JsValue::Undefined, JsValue::Number),
    );
    let defined = [input, output, cache_read, total]
        .iter()
        .any(Option::is_some);
    defined.then(|| JsValue::Object(usage))
}

// ---------------------------------------------------------------------------
// stream.ts

/// `GoogleStreamToolCallState`.
#[derive(Clone)]
struct ToolCallIdState {
    base_id: String,
    used_ids: Vec<String>,
}
impl Default for ToolCallIdState {
    fn default() -> Self {
        Self {
            base_id: "google_stream".into(),
            used_ids: Vec::new(),
        }
    }
}

/// `GoogleStreamState`.
#[derive(Default)]
pub(crate) struct StreamState {
    started: bool,
    ended: bool,
    tool_calls: ToolCallIdState,
    /// TS `rawResponse += JSON.stringify(chunk)` for the invocation audit; the
    /// accumulator's V8 limit is observable even though audit staging (pr10)
    /// is not ported yet.
    raw_response: Vec<u16>,
}
impl StreamState {
    pub(crate) fn ended(&self) -> bool {
        self.ended
    }
    pub(crate) fn normalize(&mut self, raw: &JsValue) -> Result<Vec<JsValue>, ModelStreamError> {
        normalize_stream_event(raw, self)
    }
    fn next_id(&mut self, incoming: Option<&JsString>) -> String {
        let candidate = match incoming {
            Some(id) if !id.is_empty() => {
                sanitize_tool_call_id(&String::from_utf16_lossy(id.units()), "google")
            }
            _ => format!(
                "call_{}_{}",
                self.tool_calls.base_id,
                self.tool_calls.used_ids.len()
            ),
        };
        let unique = unique_tool_call_id(&candidate, &self.tool_calls.used_ids);
        self.tool_calls.used_ids.push(unique.clone());
        unique
    }
}

fn unique_tool_call_id(id: &str, used: &[String]) -> String {
    if !used.contains(&id.to_string()) {
        return id.to_string();
    }
    let mut suffix = 2u64;
    loop {
        let candidate = format!("{id}_{suffix}");
        if !used.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn event<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut out = value_object();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

/// `generateContentResponseFromMldev` field order.
fn sdk_response(wire: &JsValue) -> JsValue {
    let Some(record) = wire.as_object() else {
        return JsValue::Object(value_object());
    };
    let mut out = value_object();
    for key in [
        "candidates",
        "modelVersion",
        "promptFeedback",
        "responseId",
        "usageMetadata",
        "modelStatus",
    ] {
        let Some(value) = record.get(key) else {
            continue;
        };
        if matches!(value, JsValue::Undefined | JsValue::Null) {
            continue;
        }
        let value = if key == "candidates" {
            match value {
                JsValue::Array(items) => JsValue::Array(items.iter().map(sdk_candidate).collect()),
                other => other.clone(),
            }
        } else {
            value.clone()
        };
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

/// `candidateFromMldev` field order (content and the passthrough fields).
fn sdk_candidate(candidate: &JsValue) -> JsValue {
    let Some(record) = candidate.as_object() else {
        return JsValue::Object(value_object());
    };
    let mut out = value_object();
    for key in [
        "content",
        "citationMetadata",
        "tokenCount",
        "finishReason",
        "groundingMetadata",
        "avgLogprobs",
        "index",
        "logprobsResult",
        "safetyRatings",
        "urlContextMetadata",
    ] {
        let Some(value) = record.get(key) else {
            continue;
        };
        if matches!(value, JsValue::Undefined | JsValue::Null) {
            continue;
        }
        let value = if key == "citationMetadata" {
            // SDK citationMetadataFromMldev: only citationSources survives,
            // renamed to citations; citation entries themselves pass through.
            let mut metadata = value_object();
            let sources = field(value, "citationSources");
            if !matches!(sources, JsValue::Undefined | JsValue::Null) {
                metadata.insert("citations".into(), sources.clone());
            }
            JsValue::Object(metadata)
        } else {
            value.clone()
        };
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

/// `normalizeGoogleStreamEvent(raw, state)`: the SDK first maps the wire JSON
/// through `generateContentResponseFromMldev`, which fixes the field order of
/// the chunk and every candidate (the normalizer's `raw` is that projection).
pub(crate) fn normalize_stream_event(
    wire: &JsValue,
    state: &mut StreamState,
) -> Result<Vec<JsValue>, ModelStreamError> {
    let raw_value = sdk_response(wire);
    let raw = &raw_value;
    let chunk = raw;
    let encoded = crate::js_value::json_stringify(&raw_value)
        .map_err(|_| ModelStreamError::Thrown {
            name: "RangeError".into(),
            message: crate::js_value::StackOverflow::MESSAGE.into(),
        })?
        .unwrap_or_default();
    let units: Vec<u16> = encoded.encode_utf16().collect();
    append_js_string(&mut state.raw_response, &units)?;
    let mut events = Vec::new();
    if !state.started {
        state.started = true;
        if let Some(response_id) = read_string(field(chunk, "responseId"))
            && !response_id.is_empty()
        {
            state.tool_calls.base_id =
                sanitize_tool_call_id(&String::from_utf16_lossy(response_id.units()), "google");
        }
        events.push(event([
            ("type", "message_start".into()),
            ("role", "assistant".into()),
            ("raw", raw.clone()),
        ]));
    }
    if let Some(usage) = normalize_usage(field(chunk, "usageMetadata")) {
        events.push(event([
            ("type", "usage".into()),
            ("usage", usage),
            ("raw", raw.clone()),
        ]));
    }
    for candidate in read_candidates(chunk) {
        for part in read_parts(candidate) {
            events.extend(part_events(part, state, raw)?);
        }
        let finish = field(candidate, "finishReason");
        if truthy(finish) {
            state.ended = true;
            events.push(event([
                ("type", "message_end".into()),
                ("finishReason", normalize_finish_reason(finish).into()),
                ("raw", raw.clone()),
            ]));
        }
    }
    Ok(events)
}

fn read_candidates(chunk: &JsValue) -> Vec<&JsValue> {
    match field(chunk, "candidates") {
        JsValue::Array(items) => items
            .iter()
            .map(|item| {
                if matches!(item, JsValue::Object(_)) {
                    item
                } else {
                    static EMPTY: JsValue = JsValue::Undefined;
                    &EMPTY
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn read_parts(candidate: &JsValue) -> Vec<&JsValue> {
    match field(field(candidate, "content"), "parts") {
        JsValue::Array(items) => items.iter().collect(),
        _ => Vec::new(),
    }
}

fn part_events(
    part: &JsValue,
    state: &mut StreamState,
    raw: &JsValue,
) -> Result<Vec<JsValue>, ModelStreamError> {
    static EMPTY: JsValue = JsValue::Undefined;
    let record = match part {
        JsValue::Object(_) => part,
        _ => &EMPTY,
    };
    let text = read_string(field(record, "text"));
    if let Some(text) = text {
        if field(record, "thought") == &JsValue::Bool(true) {
            let signature = read_string(field(record, "thoughtSignature"));
            return Ok(vec![event([
                ("type", "thinking_delta".into()),
                ("text", JsValue::String(text.clone())),
                (
                    "signature",
                    signature.map_or(JsValue::Undefined, |s| JsValue::String(s.clone())),
                ),
                ("raw", raw.clone()),
            ])]);
        }
        return Ok(if text.is_empty() {
            Vec::new()
        } else {
            vec![event([
                ("type", "text_delta".into()),
                ("text", JsValue::String(text.clone())),
                ("raw", raw.clone()),
            ])]
        });
    }
    let function_call = field(record, "functionCall");
    let has_call = function_call
        .as_object()
        .is_some_and(|object| !object.is_empty());
    if !has_call {
        return Ok(Vec::new());
    }
    let mut tool_call = value_object();
    let id = state.next_id(read_string(field(function_call, "id")));
    let name = read_string(field(function_call, "name"))
        .cloned()
        .unwrap_or_default();
    let input = match field(function_call, "args") {
        JsValue::Object(_) => field(function_call, "args").clone(),
        _ => JsValue::Object(value_object()),
    };
    tool_call.insert("id".into(), id.as_str().into());
    tool_call.insert("name".into(), JsValue::String(name.clone()));
    tool_call.insert("input".into(), input.clone());
    tool_call.insert("raw".into(), raw.clone());
    let args = crate::js_value::json_stringify(&input)
        .map_err(|_| ModelStreamError::Thrown {
            name: "RangeError".into(),
            message: crate::js_value::StackOverflow::MESSAGE.into(),
        })?
        .unwrap_or_default();
    let mut events = vec![event([
        ("type", "tool_call_start".into()),
        ("id", id.as_str().into()),
        ("name", JsValue::String(name)),
        ("raw", raw.clone()),
    ])];
    if !args.is_empty() {
        events.push(event([
            ("type", "tool_call_delta".into()),
            ("id", id.as_str().into()),
            ("delta", args.as_str().into()),
            ("raw", raw.clone()),
        ]));
    }
    events.push(event([
        ("type", "tool_call_end".into()),
        ("toolCall", JsValue::Object(tool_call)),
        ("raw", raw.clone()),
    ]));
    Ok(events)
}

// ---------------------------------------------------------------------------
// Error body handling of `client.ts` / SDK `throwErrorIfNotOK`

/// `JSON.stringify(errorBody)`: the SDK's `ApiError.message`.
pub(crate) fn api_error_message(body: &JsValue) -> Result<JsString, ModelStreamError> {
    crate::js_value::json_stringify(body)
        .map(|message| JsString::from(message.unwrap_or_default()))
        .map_err(|_| ModelStreamError::Thrown {
            name: "RangeError".into(),
            message: crate::js_value::StackOverflow::MESSAGE.into(),
        })
}

/// `JSON.stringify(ApiError)` projection: own enumerable `name` and `status`.
pub(crate) fn api_error_raw(status: u16) -> JsObject {
    let mut raw = value_object();
    raw.insert("name".into(), "ApiError".into());
    raw.insert("status".into(), JsValue::Number(f64::from(status)));
    raw
}

/// Synthesize the SDK's non-JSON error body:
/// `{error:{message:text, code:status, status:statusText}}`.
pub(crate) fn synthesized_error_body(text: &str, status: u16, status_text: &str) -> JsValue {
    let mut error = value_object();
    error.insert("message".into(), text.into());
    error.insert("code".into(), JsValue::Number(f64::from(status)));
    error.insert("status".into(), status_text.into());
    let mut body = value_object();
    body.insert("error".into(), JsValue::Object(error));
    JsValue::Object(body)
}

/// Parse a JSON error body with `Response.json()` semantics. A parse failure
/// becomes the V8 `SyntaxError` the SDK's `.json()` rejects with.
pub(crate) fn parse_error_body(text: &str) -> Result<JsValue, ModelStreamError> {
    parse_json(text).map_err(|_| ModelStreamError::Thrown {
        name: "SyntaxError".into(),
        message: pilotdeck_protocol::json_syntax::parse_error(text)
            .unwrap_or_else(|| "Unexpected end of JSON input".into())
            .into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    fn json(source: &str) -> JsValue {
        parse_json(source).expect("valid json")
    }
    fn text_of(value: &JsValue) -> String {
        pilotdeck_protocol::stringify_json(value).unwrap().unwrap()
    }

    #[test]
    fn prototype_assignments_match_real_ts_and_sdk() {
        let oracle = crate::test_baseline::oracle(include_str!(
            "../../../../tests/fixtures/model-stream-google/oracle.json"
        ));
        let schemas = array(field(&oracle, "prototypeSchemaCases"));
        assert_eq!(schemas.len(), 17);
        for case in schemas {
            let schema = field(case, "schema");
            assert_eq!(
                text_of(&clean_schema_for_google(schema)),
                text_of(field(case, "cleaned")),
                "{}: clean",
                text_of(field(case, "name"))
            );
            assert_eq!(
                text_of(&normalize_google_tool_schema(schema)),
                text_of(field(case, "normalized")),
                "{}: normalize",
                text_of(field(case, "name"))
            );
        }
        let merges = array(field(&oracle, "prototypeMergeCases"));
        assert_eq!(merges.len(), 8);
        for case in merges {
            let actual = deep_merge(
                field(case, "target").as_object().unwrap().clone(),
                field(case, "source").as_object().unwrap(),
            );
            assert_eq!(
                text_of(&JsValue::Object(actual)),
                text_of(field(case, "expected")),
                "{}: SDK merge",
                text_of(field(case, "name"))
            );
        }
    }

    // Keep this probe isolated: reverting to native recursion can abort a process.
    #[test]
    #[ignore = "2 MiB stack probe; run explicitly in debug and release"]
    fn schema_two_mib_stack_probe() {
        std::thread::Builder::new()
            .name("google-schema-2mib".into())
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let oracle = crate::test_baseline::oracle(include_str!(
                    "../../../../tests/fixtures/model-stream-google/oracle.json"
                ));
                let cases = array(field(&oracle, "schemaCases"));
                assert_eq!(cases.len(), 4);
                for case in cases {
                    let depth = field(case, "depth").as_number().unwrap() as usize;
                    let mut schema = json(r#"{"const":"leaf"}"#);
                    for _ in 0..depth {
                        let mut properties = JsObject::new();
                        properties.insert("a".into(), schema);
                        let mut node = JsObject::new();
                        node.insert("properties".into(), JsValue::Object(properties));
                        schema = JsValue::Object(node);
                    }
                    let mut tool = JsObject::new();
                    tool.insert("name".into(), "deep".into());
                    tool.insert("inputSchema".into(), schema);
                    let mut request = JsObject::new();
                    request.insert("tools".into(), JsValue::Array(vec![JsValue::Object(tool)]));
                    eprintln!("stack_bytes=2097152 properties_depth={depth} phase=build_request");
                    let result =
                        build_request(&JsValue::Object(request), &json(r#"{"id":"gemini-3-pro"}"#))
                            .unwrap();
                    let declaration = &array(field(
                        &array(field(&result, "tools"))[0],
                        "functionDeclarations",
                    ))[0];
                    let mut leaf = field(declaration, "parametersJsonSchema");
                    let mut traversed = 0;
                    while !field(field(leaf, "properties"), "a").is_undefined() {
                        leaf = field(field(leaf, "properties"), "a");
                        traversed += 1;
                    }
                    assert_eq!(
                        traversed,
                        field(case, "traversed").as_number().unwrap() as usize
                    );
                    assert_eq!(text_of(leaf), text_of(field(case, "leaf")));
                    eprintln!("returned=Ok traversed={traversed} leaf={}", text_of(leaf));
                }
                // The other two former recursive walks must also survive 2 MiB.
                let mut union = json(r#"{"const":"leaf"}"#);
                for _ in 0..2000 {
                    let mut node = JsObject::new();
                    node.insert("anyOf".into(), JsValue::Array(vec![union]));
                    union = JsValue::Object(node);
                }
                assert_eq!(extract_enum_values(&union), Some(vec!["leaf".into()]));
                assert_eq!(
                    text_of(&clean_schema_for_google(&union)),
                    text_of(field(cases.last().unwrap(), "union"))
                );
                eprintln!("union_depth=2000 extract_enum_values=Ok clean_schema=Ok");
                let mut target = json(r#"{"keep":true,"value":"old"}"#);
                let mut source = json(r#"{"value":"new","add":true}"#);
                for _ in 0..2000 {
                    let wrap = |value| {
                        let mut node = JsObject::new();
                        node.insert("a".into(), value);
                        JsValue::Object(node)
                    };
                    target = wrap(target);
                    source = wrap(source);
                }
                let result = JsValue::Object(deep_merge(
                    target.as_object().unwrap().clone(),
                    source.as_object().unwrap(),
                ));
                let mut leaf = &result;
                for _ in 0..2000 {
                    leaf = field(leaf, "a");
                }
                assert_eq!(text_of(leaf), r#"{"keep":true,"value":"new","add":true}"#);
                eprintln!("deep_merge_depth=2000 leaf={}", text_of(leaf));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// Socket-free replay of the actual SDK projection recorded by TS.
    #[test]
    fn citation_metadata_matches_real_ts_events() {
        let oracle = crate::test_baseline::oracle(include_str!(
            "../../../../tests/fixtures/model-stream-google/oracle.json"
        ));
        let cases: Vec<_> = array(field(&oracle, "cases"))
            .iter()
            .filter(|case| is(field(case, "name"), "citation-metadata"))
            .collect();
        assert_eq!(cases.len(), 1);
        let case = cases[0];
        let mut state = StreamState::default();
        let mut events = Vec::new();
        for reply in array(field(case, "replies")) {
            for chunk in array(field(reply, "chunks")) {
                let text = field(chunk, "text").as_string().unwrap().to_utf8().unwrap();
                let wire = json(text.strip_prefix("data: ").unwrap().trim());
                events.extend(normalize_stream_event(&wire, &mut state).unwrap());
            }
        }
        let expected: Vec<_> = array(field(case, "timeline"))
            .iter()
            .map(|entry| field(entry, "event"))
            .filter(|event| !is(field(event, "type"), "request_started"))
            .cloned()
            .collect();
        assert_eq!(events.len(), 3);
        assert_eq!(
            text_of(&JsValue::Array(events)),
            text_of(&JsValue::Array(expected))
        );
    }

    #[test]
    fn model_ids_match_the_baseline_table() {
        assert_eq!(normalize_model_id("gemini-3-pro"), "gemini-3-pro-preview");
        assert_eq!(
            normalize_model_id(" google/gemini-3-flash "),
            "gemini-3-flash-preview"
        );
        assert_eq!(
            normalize_model_id("gemini-3.1-flash-preview"),
            "gemini-3-flash-preview"
        );
        assert_eq!(normalize_model_id("models/gemini-2"), "models/gemini-2");
    }

    #[test]
    fn tool_call_ids_are_sanitized_like_the_baseline() {
        assert_eq!(sanitize_tool_call_id("  a b  ", "call"), "a_b");
        assert_eq!(sanitize_tool_call_id("a-b__c_", "call"), "a-b__c");
        assert_eq!(sanitize_tool_call_id("  ?? ", "call"), "call");
        assert_eq!(sanitize_tool_call_id("a/b~c", "google"), "a_b_c");
    }

    #[test]
    fn schema_cleaning_matches_the_generated_request_boundaries() {
        let schema = json(
            r##"{"$defs":{"Item":{"type":"object","properties":{"id":{"type":"integer"}},"required":["id","gone"]}},
                "title":"Root","description":"root description","type":["object","null"],
                "properties":{"answer":{"type":["string","null"],"minLength":2,"description":"answer","default":"x"},
                "mode":{"anyOf":[{"const":"a"},{"const":"b"},{"type":"null"}]},
                "item":{"$ref":"#/$defs/Item"},
                "list":{"type":"array","minItems":1},
                "nested":{"type":"object","properties":{"bad-name":{"type":"string"},"good_name":{"type":"string"}},"additionalProperties":false},
                "scalar":{"const":7},"any":{}},
                "required":["answer","missing","bad-name","mode"],"additionalProperties":false,
                "patternProperties":{"^x":{"type":"string"}},"examples":[{"answer":"x"}],
                "$schema":"https://json-schema.org/draft/2020-12/schema"}"##,
        );
        assert_eq!(
            text_of(&normalize_google_tool_schema(&schema)),
            r#"{"title":"Root","description":"root description","type":"object","properties":{"answer":{"type":"string","description":"answer","default":"x"},"mode":{"type":"string","enum":["a","b"]},"item":{"type":"object","properties":{"id":{"type":"integer"}},"required":["id"]},"list":{"type":"array","items":{}},"nested":{"type":"object","properties":{"good_name":{"type":"string"}}},"scalar":{"enum":[7]},"any":{}},"required":["answer","mode"]}"#
        );
    }

    #[test]
    fn top_level_object_union_flattens_literal_properties() {
        let schema = json(
            r##"{"anyOf":[
                {"type":"object","properties":{"kind":{"const":"a"},"x":{"type":"string"}},"required":["kind"]},
                {"type":"object","properties":{"kind":{"const":"b"},"y":{"type":"number"}},"required":["kind"]}
            ]}"##,
        );
        assert_eq!(
            text_of(&normalize_google_tool_schema(&schema)),
            r#"{"type":"object","properties":{"kind":{"enum":["a","b"],"type":"string"},"x":{"type":"string"},"y":{"type":"number"}},"required":["kind"]}"#
        );
    }

    #[test]
    fn thinking_plan_keeps_plan_enabled_for_foreign_formats() {
        // GLM round 1 P2-1: `resolveThinkingPlan` starts from `enabled: !off`
        // for every format, so a disabled google-protocol model resolves to
        // `enabled: false` even when the parameter format is openai/anthropic.
        let request = json(r#"{"thinking":{"mode":"high"}}"#);
        for format in ["openai", "anthropic"] {
            let model = json(&format!(
                r#"{{"id":"gemini-3-pro","thinking":{{"state":"disabled","efforts":["low"],"format":"{format}"}}}}"#
            ));
            let plan = thinking_plan(&request, &model).expect("disabled plan");
            assert!(!plan.enabled, "{format}: disabled must stay disabled");
            assert!(
                thinking_config(plan).is_none(),
                "{format}: no thinkingConfig"
            );
        }
        let model = json(
            r#"{"id":"gemini-3-pro","thinking":{"state":"enabled","efforts":["high"],"format":"openai"}}"#,
        );
        let plan = thinking_plan(&request, &model).expect("enabled plan");
        assert!(plan.enabled);
        assert_eq!(
            text_of(&thinking_config(plan).expect("thinkingConfig")),
            r#"{"includeThoughts":true}"#
        );
    }

    /// Real TS `buildGoogleRequest` probes for the fields added to the shared
    /// registry during the pr8 move; default/unsupported-model plans must not
    /// accidentally acquire `includeThoughts`.
    #[test]
    fn shared_thinking_plan_preserves_google_levels_and_disabled_defaults() {
        let default_request = json(r#"{"thinking":{"mode":"default"}}"#);
        for (id, state, format) in [
            ("gemini-3-pro", "default", "provider"),
            ("gemini-2.5-pro", "enabled", "provider"),
            ("gemini-3-pro", "enabled", "server-default"),
            ("claude-sonnet-4", "enabled", "anthropic"),
        ] {
            let model = json(&format!(
                r#"{{"id":"{id}","thinking":{{"state":"{state}","efforts":[],"format":"{format}"}}}}"#
            ));
            assert!(thinking_config(thinking_plan(&default_request, &model).unwrap()).is_none());
        }
        let model = json(
            r#"{"id":"gemini-3-pro","thinking":{"state":"enabled","efforts":["high"],"format":"provider"}}"#,
        );
        for (request, expected) in [
            (default_request, r#"{"includeThoughts":true}"#),
            (
                json(r#"{"thinking":{"mode":"high"}}"#),
                r#"{"includeThoughts":true,"thinkingLevel":"high"}"#,
            ),
        ] {
            assert_eq!(
                text_of(&thinking_config(thinking_plan(&request, &model).unwrap()).unwrap()),
                expected
            );
        }
    }

    #[test]
    fn finish_reasons_and_usage_follow_response_ts() {
        assert_eq!(normalize_finish_reason(&json("\"stop\"")), "stop");
        assert_eq!(normalize_finish_reason(&json("\"max_tokens\"")), "length");
        assert_eq!(
            normalize_finish_reason(&json("\"MALFORMED_FUNCTION_CALL\"")),
            "tool_call"
        );
        assert_eq!(
            normalize_finish_reason(&json("\"SAFETY\"")),
            "content_filter"
        );
        for reason in [
            "RECITATION",
            "BLOCKLIST",
            "PROHIBITED_CONTENT",
            "SPII",
            "IMAGE_SAFETY",
        ] {
            assert_eq!(
                normalize_finish_reason(&JsValue::from(reason)),
                "content_filter",
                "{reason}"
            );
        }
        assert_eq!(
            normalize_finish_reason(&json("\"UNEXPECTED_TOOL_CALL\"")),
            "tool_call"
        );
        assert_eq!(normalize_finish_reason(&json("\"other\"")), "unknown");
        let usage = normalize_usage(&json(
            r#"{"promptTokenCount":10,"cachedContentTokenCount":4,"candidatesTokenCount":3,"thoughtsTokenCount":2}"#,
        ))
        .expect("usage");
        assert_eq!(
            text_of(&usage),
            r#"{"inputTokens":6,"outputTokens":5,"cacheReadTokens":4,"totalTokens":15}"#
        );
        // Real TS probes: Google differs from OpenAI on the cache floor and
        // finite operands whose sum overflows (JSON.stringify emits null).
        for (raw, expected) in [
            (
                r#"{"promptTokenCount":3,"cachedContentTokenCount":5,"candidatesTokenCount":7,"thoughtsTokenCount":2}"#,
                r#"{"inputTokens":0,"outputTokens":9,"cacheReadTokens":5,"totalTokens":12}"#,
            ),
            (
                r#"{"candidatesTokenCount":1e308,"thoughtsTokenCount":1e308}"#,
                r#"{"outputTokens":null,"totalTokens":null}"#,
            ),
            (
                r#"{"cachedContentTokenCount":4}"#,
                r#"{"cacheReadTokens":4}"#,
            ),
        ] {
            assert_eq!(text_of(&normalize_usage(&json(raw)).unwrap()), expected);
        }
    }

    #[test]
    fn raw_chunk_accumulation_uses_the_v8_string_limit() {
        let _limit = crate::runtime::model::http::LimitGuard::set(16);
        let mut state = StreamState::default();
        let raw = json(r#"{"candidates":[{"content":{"parts":[{"text":"long enough"}]}}]}"#);
        let error = normalize_stream_event(&raw, &mut state).unwrap_err();
        assert!(matches!(
            error,
            ModelStreamError::Thrown { ref name, ref message }
                if name.equals_str("RangeError") && message.equals_str("Invalid string length")
        ));
    }

    #[test]
    fn stream_state_dedupes_ids_and_normalizes_parts() {
        let mut state = StreamState::default();
        let raw = json(
            r#"{"responseId":"resp/one","candidates":[{"content":{"parts":[
                {"functionCall":{"id":"a b","name":"lookup","args":{"q":"x"}}},
                {"functionCall":{"id":"a_b","name":"lookup","args":{"q":"y"}}}
            ]},"finishReason":"STOP"}]}"#,
        );
        let events: Vec<String> = normalize_stream_event(&raw, &mut state)
            .unwrap()
            .iter()
            .map(text_of)
            .collect();
        assert!(events[0].contains("\"message_start\""));
        assert!(events.iter().any(|event| event.contains("\"a_b\"")));
        assert!(events.iter().any(|event| event.contains("\"a_b_2\"")));
        let projected = text_of(&sdk_response(&raw));
        assert_eq!(
            events.last().unwrap(),
            &format!(r#"{{"type":"message_end","finishReason":"stop","raw":{projected}}}"#)
        );
    }
}
