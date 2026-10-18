//! Whole-batch safe-first scheduling; results retain caller order.
use super::{
    registry::{self, ToolRegistry},
    *,
};
use crate::host::{
    HostFuture,
    modules::{
        capability::{ToolCall, ToolCatalog, ToolDefinition, ToolExecution, ToolInvocationContext},
        permission::PermissionDecision,
        tool_context::ExecutionContext,
    },
};
use std::{future::poll_fn, task::Poll};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SchedulerMode {
    Sequential,
    #[default]
    Concurrent,
}

/// Permission policy is injected explicitly. Lifecycle/plan observers are
/// separate service owners; this layer owns registry resolution and execution.
#[derive(Clone)]
pub struct ToolRuntime {
    pub registry: ToolRegistry,
    permission: Arc<dyn PermissionDecision>,
    pub mode: SchedulerMode,
}
impl ToolRuntime {
    pub fn new(
        registry: ToolRegistry,
        permission: Arc<dyn PermissionDecision>,
        mode: SchedulerMode,
    ) -> Self {
        Self {
            registry,
            permission,
            mode,
        }
    }
    pub async fn execute_batch(
        &self,
        calls: Vec<ToolCall>,
        context: ToolRunContext,
    ) -> Result<Vec<JsObject>, HostError> {
        let mut concurrent = Vec::new();
        let mut sequential = Vec::new();
        // Predicates are evaluated for the entire batch before any call starts.
        // Single-call batches bypass the predicate, exactly like TS.
        for (index, call) in calls.iter().enumerate() {
            let safe = self.mode == SchedulerMode::Concurrent
                && calls.len() > 1
                && self
                    .registry
                    .get(&call.name)
                    .map(|t| t.is_concurrency_safe(&call.input))
                    .transpose()?
                    .is_some_and(|v| truthy(&v));
            if safe {
                concurrent.push(index);
            } else {
                sequential.push(index);
            }
        }
        let mut slots = vec![None; calls.len()];
        // Owned futures, not JoinHandles: early error, host abort, timeout and
        // Drop synchronously destroy every unfinished sibling future.
        let mut pending: Vec<_> = concurrent
            .into_iter()
            .map(|index| {
                (
                    index,
                    Some(Box::pin(
                        self.execute(calls[index].clone(), context.clone()),
                    )),
                )
            })
            .collect();
        poll_fn(|cx| {
            let mut unfinished = false;
            for (index, slot) in &mut pending {
                let Some(future) = slot else {
                    continue;
                };
                match future.as_mut().poll(cx) {
                    Poll::Ready(Ok(result)) => {
                        slots[*index] = Some(result);
                        *slot = None;
                    }
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => unfinished = true,
                }
            }
            if unfinished {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await?;
        for index in sequential {
            slots[index] = Some(self.execute(calls[index].clone(), context.clone()).await?);
        }
        Ok(slots
            .into_iter()
            .map(|v| v.expect("every call has a result slot"))
            .collect())
    }
    pub async fn execute(
        &self,
        call: ToolCall,
        context: ToolRunContext,
    ) -> Result<JsObject, HostError> {
        let started = context.now_iso()?;
        let mut tool = self.registry.get(&call.name);
        let mut repaired = None;
        if tool.is_none() {
            let mut targets: Vec<_> = self
                .registry
                .list()?
                .iter()
                .map(|t| (registry::name(t.as_ref()), registry::aliases(t.as_ref())))
                .collect();
            targets.extend(
                self.registry
                    .unavailable_entries()?
                    .into_iter()
                    .map(|e| (e.diagnostic.name, e.aliases)),
            );
            repaired = registry::repair_tool_name(
                &call.name,
                &targets,
                context
                    .fields
                    .get("toolAliases")
                    .and_then(JsValue::as_object),
            )?
            .map(|r| r.name);
            tool = repaired.as_ref().and_then(|name| self.registry.get(name));
        }
        let name = tool
            .as_ref()
            .map(|t| registry::name(t.as_ref()))
            .unwrap_or_else(|| call.name.clone());
        let fail = |name: &JsString, error: ToolError| {
            create_tool_error_result(&call.id, name, &error, &started, &context)
        };
        if context.is_aborted() {
            return fail(&name, ToolError::aborted());
        }
        if tool.is_some() && self.registry.is_hidden(&name) {
            return fail(
                &call.name,
                ToolError::new(
                    "tool_not_found",
                    named(
                        "Tool ",
                        &call.name,
                        " is deferred for this session. Use search_tools to load it before calling it.",
                    ),
                ),
            );
        }
        let Some(tool) = tool else {
            if let Some(unavailable) = self
                .registry
                .get_unavailable(repaired.as_ref().unwrap_or(&call.name))
            {
                let mut error = ToolError::new(
                    if unavailable.code.equals_str("setup_required") {
                        "setup_required"
                    } else {
                        "tool_unavailable"
                    },
                    unavailable.reason.clone(),
                );
                error.details = Some(object([
                    ("availabilityCode", string(unavailable.code)),
                    ("reason", string(unavailable.reason)),
                ]));
                return fail(&call.name, error);
            }
            return fail(
                &call.name,
                ToolError::new(
                    "tool_not_found",
                    named("Tool ", &call.name, " does not exist."),
                ),
            );
        };
        let operation = async {
            let issues = validate_input(
                &call.input,
                tool.fields()
                    .get("inputSchema")
                    .unwrap_or(&JsValue::Undefined),
            );
            if !issues.is_empty() {
                return fail(&name, validation_error(&name, issues, &context));
            }
            let prompt = name.equals_str("enter_plan_mode")
                || tool.requires_user_interaction(&call.input).ok() == Some(JsValue::Bool(true));
            let can_elicit = name.equals_str("ask_user_question")
                && context.fields.get("canElicit") == Some(&JsValue::Bool(true))
                && context.host.elicitation.is_some();
            if context
                .fields
                .get("permissionContext")
                .and_then(|v| v.get("canPrompt"))
                == Some(&JsValue::Bool(false))
                && prompt
                && !can_elicit
            {
                return fail(
                    &name,
                    ToolError::new(
                        "unsupported_tool",
                        named(
                            "",
                            &name,
                            " requires user interaction, but this session is running with prompts disabled.",
                        ),
                    ),
                );
            }
            match tool
                .validate_input(call.input.clone(), context.clone())
                .await
            {
                Err(error) => return fail(&name, error),
                Ok(Some(issues)) => return fail(&name, validation_error(&name, issues, &context)),
                Ok(None) => {}
            }
            let definition: Arc<dyn ToolDefinition> = tool.clone();
            let decision = self
                .permission
                .decide(
                    definition,
                    call.input.clone(),
                    Arc::new(context.clone()),
                    call.id.clone(),
                )
                .await?;
            let kind = decision.get("type").and_then(JsValue::as_string);
            let message = decision
                .get("message")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default();
            match kind.map(text).as_deref() {
                Some("deny") => {
                    let runtime = decision
                        .get("reason")
                        .and_then(|r| r.get("type"))
                        .and_then(JsValue::as_string)
                        .is_some_and(|s| s.equals_str("runtime"));
                    let prompt = decision
                        .get("reason")
                        .and_then(|r| r.get("message"))
                        .and_then(JsValue::as_string)
                        .is_some_and(|s| text(s).contains("prompt"));
                    return fail(
                        &name,
                        ToolError::new(
                            if runtime && prompt {
                                "permission_required"
                            } else {
                                "permission_denied"
                            },
                            message,
                        ),
                    );
                }
                Some("cancel") => {
                    return fail(&name, ToolError::new("permission_cancelled", message));
                }
                Some("ask") => {
                    let mut error = ToolError::new(
                        "permission_required",
                        named("Permission is required to run ", &name, "."),
                    );
                    error.details = Some(object([(
                        "request",
                        decision
                            .get("request")
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    )]));
                    return fail(&name, error);
                }
                // TS executes for "allow" and for any other decision type.
                _ => {}
            }
            let input = decision
                .get("updatedInput")
                .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
                .cloned()
                .unwrap_or_else(|| call.input.clone());
            let mut execution = context.clone();
            execution
                .fields
                .insert("currentToolCallId".into(), string(call.id.clone()));
            execution
                .fields
                .insert("currentPermissionDecision".into(), decision);
            match tool.execute(input, execution).await {
                Err(error) => fail(&name, error),
                Ok(output) => {
                    let mut metadata = output.metadata;
                    let max_bytes = tool
                        .fields()
                        .get("maxResultBytes")
                        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                        .or_else(|| context.fields.get("maxResultBytes"))
                        .and_then(JsValue::as_number);
                    if let Some(limit) = preview_metadata(&output.content, max_bytes) {
                        metadata
                            .get_or_insert_with(JsObject::new)
                            .insert("previewLimit".into(), JsValue::Object(limit));
                    }
                    Ok(object([
                        ("type", "success".into()),
                        ("toolCallId", string(call.id.clone())),
                        ("toolName", string(name.clone())),
                        ("content", JsValue::Array(output.content)),
                        (
                            "supplementalMessages",
                            output
                                .supplemental_messages
                                .map(JsValue::Array)
                                .unwrap_or(JsValue::Undefined),
                        ),
                        ("data", output.data.unwrap_or(JsValue::Undefined)),
                        (
                            "metadata",
                            metadata.map(JsValue::Object).unwrap_or(JsValue::Undefined),
                        ),
                        ("startedAt", string(started.clone())),
                        ("completedAt", string(context.now_iso()?)),
                    ]))
                }
            }
        };
        // Like TS, abort is cooperative: a started tool observes the signal and
        // settles itself. The batch future owns it, so dropping the batch (host
        // operation deadline) still destroys it without detached work.
        operation.await
    }
}
impl ToolCatalog for ToolRuntime {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        let registry = self.registry.clone();
        Box::pin(async move {
            Ok(registry
                .list()?
                .into_iter()
                .map(|t| t as Arc<dyn ToolDefinition>)
                .collect())
        })
    }
}
impl ToolExecution for ToolRuntime {
    fn execute_all(
        &self,
        calls: Vec<ToolCall>,
        context: ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        let runtime = self.clone();
        Box::pin(async move {
            let host = context
                .runtime
                .downcast::<ToolRuntimeContext>()
                .map_err(|_| {
                    ModuleError::new("Tool runtime requires a host ToolRuntimeContext.")
                })?;
            let execution = context
                .execution
                .downcast::<ExecutionContext>()
                .map_err(|_| ModuleError::new("Tool runtime requires a host ExecutionContext."))?;
            let mut context = ToolRunContext::new(host);
            with_operation_deadline_budget(&mut context, &execution.fields)?;
            runtime.execute_batch(calls, context).await
        })
    }
}

fn validate_input(value: &JsValue, schema: &JsValue) -> Vec<JsObject> {
    fn issue(out: &mut Vec<JsObject>, path: &str, code: &str, message: String) {
        out.push(object([
            ("path", path.into()),
            ("code", code.into()),
            ("message", string(message)),
        ]));
    }
    fn matches(value: &JsValue, kind: &JsValue) -> bool {
        if let JsValue::Array(kinds) = kind {
            return kinds.iter().any(|k| matches(value, k));
        }
        match kind.as_string().map(text).as_deref() {
            Some("null") => matches!(value, JsValue::Null),
            Some("object") => matches!(value, JsValue::Object(_)),
            Some("array") => matches!(value, JsValue::Array(_)),
            Some("string") => value.as_string().is_some(),
            Some("boolean") => value.as_bool().is_some(),
            Some("number") => value.as_number().is_some_and(f64::is_finite),
            Some("integer") => value
                .as_number()
                .is_some_and(|n| n.is_finite() && n.fract() == 0.0),
            _ => false,
        }
    }
    /// `key in object` for a JSON-shaped object whose chain ends at
    /// Object.prototype: own keys, explicit prototypes, then the 12 built-ins.
    fn has(object: &JsObject, key: &JsString) -> bool {
        object.get_key(key).is_some()
            || object.get_property(&text(key)).is_some()
            || crate::config::builtin_property(key)
    }
    /// A key found only on Object.prototype reads as a built-in function.
    fn property<'a>(object: &'a JsObject, key: &JsString) -> Option<&'a JsValue> {
        object
            .get_key(key)
            .or_else(|| object.get_property(&text(key)))
    }
    /// `None` is an inherited Object.prototype method: it equals no enum
    /// member, matches no JSON type and is not a plain object or array.
    fn walk(value: Option<&JsValue>, schema: &JsValue, path: &str, out: &mut Vec<JsObject>) {
        if let Some(JsValue::Array(enumeration)) = schema.get("enum") {
            let equal = |v: &JsValue| match (value, v) {
                (Some(JsValue::Number(a)), JsValue::Number(b)) => {
                    a.to_bits() == b.to_bits() || a.is_nan() && b.is_nan()
                }
                (None | Some(JsValue::Object(_) | JsValue::Array(_)), _) => false,
                (Some(value), _) => v == value,
            };
            if !enumeration.iter().any(equal) {
                issue(
                    out,
                    path,
                    "invalid_enum",
                    format!(
                        "{path} must be one of {}.",
                        enumeration
                            .iter()
                            .map(js_display)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
                return;
            }
        }
        let kind = schema.get("type");
        if let Some(kind) = kind
            && !value.is_some_and(|value| matches(value, kind))
        {
            let description = match kind {
                JsValue::Array(kinds) => kinds
                    .iter()
                    .map(js_display)
                    .collect::<Vec<_>>()
                    .join(" or "),
                kind => js_display(kind),
            };
            issue(
                out,
                path,
                "invalid_type",
                format!("{path} must be {description}."),
            );
            return;
        }
        let Some(value) = value else {
            return;
        };
        let effective = match kind {
            Some(JsValue::Array(kinds)) => kinds
                .iter()
                .find(|k| k.as_string().is_none_or(|s| !s.equals_str("null"))),
            kind => kind,
        };
        if (effective.is_none()
            || effective
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("object")))
            && let Some(value) = value.as_object()
        {
            let empty = JsObject::new();
            let properties = schema
                .get("properties")
                .and_then(JsValue::as_object)
                .unwrap_or(&empty);
            if let Some(JsValue::Array(required)) = schema.get("required") {
                for key in required.iter().filter_map(JsValue::as_string) {
                    if !has(value, key) {
                        let path = format!("{path}.{}", text(key));
                        issue(out, &path, "required", format!("{path} is required."));
                    }
                }
            }
            if schema.get("additionalProperties") == Some(&JsValue::Bool(false)) {
                for (key, _) in value.entries() {
                    if !has(properties, key) {
                        let path = format!("{path}.{}", text(key));
                        issue(
                            out,
                            &path,
                            "unknown_property",
                            format!("{path} is not allowed."),
                        );
                    }
                }
            }
            for (key, schema) in properties.entries() {
                if has(value, key) {
                    walk(
                        property(value, key),
                        schema,
                        &format!("{path}.{}", text(key)),
                        out,
                    );
                }
            }
        }
        if effective
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("array"))
            && let (JsValue::Array(values), Some(schema)) = (value, schema.get("items"))
        {
            for (i, value) in values.iter().enumerate() {
                walk(Some(value), schema, &format!("{path}[{i}]"), out);
            }
        }
    }
    let mut issues = vec![];
    walk(Some(value), schema, "$", &mut issues);
    issues
}
fn validation_error(name: &JsString, issues: Vec<JsObject>, context: &ToolRunContext) -> ToolError {
    let tool = text(name);
    let mut parts = vec![];
    for issue in &issues {
        let path = text(&string_field(issue, "path"));
        let param = path
            .strip_prefix('$')
            .map_or(path.as_str(), |rest| rest.strip_prefix('.').unwrap_or(rest));
        let message = text(&string_field(issue, "message"));
        parts.push(match text(&string_field(issue, "code")).as_str() {
            "required" => format!("The required parameter `{param}` is missing"),
            "invalid_type" => format!("The parameter `{param}` has an invalid type: {message}"),
            "unknown_property" => format!("An unexpected parameter `{param}` was provided"),
            "invalid_enum" => format!("The parameter `{param}` has an invalid value: {message}"),
            _ => message,
        });
    }
    if parts.is_empty() {
        return ToolError::new(
            "invalid_tool_input",
            format!("Tool {tool} input is invalid."),
        )
        .with_details(issues);
    }
    let mut message = format!(
        "{tool} failed due to the following {}:\n{}",
        if parts.len() > 1 { "issues" } else { "issue" },
        parts.join("\n")
    );
    let required = issues
        .iter()
        .any(|i| string_field(i, "code").equals_str("required"));
    if required && ["write_file", "edit_file", "edit_notebook", "bash"].contains(&tool.as_str()) {
        if context.fields.get("outputTruncated").is_some_and(truthy) {
            let tokens = context
                .fields
                .get("maxOutputTokens")
                .filter(|v| truthy(v))
                .map(|v| format!(" (current max_output_tokens: {})", js_display(v)))
                .unwrap_or_default();
            message += &format!(
                "\n\nThis was caused by your output being truncated (output token limit reached{tokens}). Keep each tool call's arguments well within the output token budget."
            );
        } else {
            message += "\n\nPlease ensure all required parameters are provided in the tool call.";
        }
    }
    if tool == "write_file"
        && issues.iter().any(|i| {
            string_field(i, "code").equals_str("required")
                && text(&string_field(i, "path")).contains("content")
        })
    {
        message += "\n\nHint: For large files, recover by creating a smaller but valid draft workspace file first. Use write_file with a complete content field that fits comfortably within the output budget. After the draft exists, read it with read_file and extend or patch it using small focused write_file/edit_file calls. Do not use shell heredocs or paths outside the workspace.";
    }
    ToolError::new("invalid_tool_input", message).with_details(issues)
}
fn js_display(v: &JsValue) -> String {
    match v {
        JsValue::String(s) => text(s),
        JsValue::Undefined => "undefined".into(),
        JsValue::Object(_) => "[object Object]".into(),
        JsValue::Array(a) => a
            .iter()
            .map(|v| {
                if matches!(v, JsValue::Null | JsValue::Undefined) {
                    String::new()
                } else {
                    js_display(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        _ => pilotdeck_protocol::stringify_json(v)
            .ok()
            .flatten()
            .unwrap_or_default(),
    }
}
fn content_text(v: &JsValue) -> String {
    let field = |key| {
        v.get(key)
            .map(js_display)
            .unwrap_or_else(|| "undefined".into())
    };
    match v
        .get("type")
        .and_then(JsValue::as_string)
        .map(text)
        .as_deref()
    {
        Some("text") => field("text"),
        Some("json") => {
            pilotdeck_protocol::stringify_json(v.get("value").unwrap_or(&JsValue::Undefined))
                .ok()
                .flatten()
                .unwrap_or_default()
        }
        Some("file") => format!(
            "[File: {}{}{}]",
            field("path"),
            v.get("mimeType")
                .filter(|v| truthy(v))
                .map(|v| format!(", {}", js_display(v)))
                .unwrap_or_default(),
            v.get("description")
                .filter(|v| truthy(v))
                .map(|v| format!(", {}", js_display(v)))
                .unwrap_or_default()
        ),
        _ => String::new(),
    }
}
fn preview_metadata(content: &[JsValue], max: Option<f64>) -> Option<JsObject> {
    let max = max.filter(|n| *n >= 0.0)?;
    let binary = |v: &JsValue| {
        v.get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("image") || s.equals_str("pdf"))
    };
    let original: usize = content
        .iter()
        .map(|v| {
            if binary(v) {
                v.get("data")
                    .and_then(JsValue::as_string)
                    .map_or(0, |s| s.units().len())
            } else {
                content_text(v).len()
            }
        })
        .sum();
    if content.iter().any(binary) {
        return Some(object([(
            "originalBytes",
            JsValue::Number(original as f64),
        )]));
    }
    if original as f64 <= max {
        return None;
    }
    let suffix = "\n[Tool output truncated: head and tail shown.]";
    let budget = (max - suffix.len() as f64).max(0.0) as usize;
    let value = content
        .iter()
        .map(content_text)
        .collect::<Vec<_>>()
        .join("\n");
    let prefix = |budget: usize| {
        let mut end = budget.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        &value[..end]
    };
    let returned = if value.len() <= budget {
        value.len()
    } else if budget <= 64 {
        prefix(budget).len()
    } else {
        let separator = "\n... [middle omitted] ...\n";
        let budget = budget.saturating_sub(separator.len());
        let mut start = value.len().saturating_sub(budget / 2);
        while !value.is_char_boundary(start) {
            start += 1;
        }
        prefix(budget.div_ceil(2)).len() + separator.len() + value.len() - start
    } + suffix.len();
    Some(object([
        ("truncated", JsValue::Bool(true)),
        ("originalBytes", JsValue::Number(original as f64)),
        ("returnedBytes", JsValue::Number(returned as f64)),
    ]))
}
