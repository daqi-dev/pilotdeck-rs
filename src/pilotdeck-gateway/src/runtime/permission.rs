//! Permission decision engine, ported from the fixed TypeScript baseline
//! (`src/permission/`):
//!
//! - `protocol/types.ts` constants and the permission context shape
//! - `policy/matchPermissionRule.ts` rule matching
//! - `decision/PermissionRuntime.ts` decision composition
//!
//! Rules are the live `PermissionRuleSet` owned by the A6.3-pr6 session
//! registry (`crate::config::permission`); this module does not add storage or
//! change that registry's public shape. The engine is a pure decision step:
//! path handling is lexical (`node:path` semantics, no filesystem access) and
//! tool predicates/`checkPermissions` are injected through [`PermissionTool`].
//!
//! All path and pattern work is done on UTF-16 units, so lone surrogates from
//! the wire survive unchanged and JS regex/`String#trim` semantics are not
//! lost to UTF-8 conversion.

use crate::config::permission::{PermissionMode, PermissionRule, PermissionRuleSet};
use crate::host::{HostError, HostErrorKind};
use crate::js_value;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin};

/// `PERMISSION_MODES` from `permission/protocol/types.ts`.
pub const PERMISSION_MODES: [&str; 3] = ["default", "plan", "bypassPermissions"];

/// `DEFAULT_PERMISSION_MODE`.
pub const DEFAULT_PERMISSION_MODE: &str = "default";

/// `isPermissionMode`: exact, case-sensitive membership in `PERMISSION_MODES`.
pub fn is_permission_mode(value: &JsValue) -> bool {
    value
        .as_string()
        .is_some_and(|value| PERMISSION_MODES.iter().any(|mode| value.equals_str(mode)))
}

/// `emptyPermissionRuleSet`.
pub fn empty_permission_rule_set() -> PermissionRuleSet {
    PermissionRuleSet::default()
}

/// `PermissionContext`: `policyCanPrompt` only ever carries `false` in the
/// baseline, so `Some(false)` is the restrictive case and `None` is omitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionContext {
    pub mode: PermissionMode,
    pub rules: PermissionRuleSet,
    pub cwd: JsString,
    pub additional_working_directories: Vec<JsString>,
    pub can_prompt: bool,
    pub policy_can_prompt: Option<bool>,
    pub bypass_available: bool,
    pub accept_edits: Option<bool>,
    pub plan_directory_path: Option<JsString>,
}

impl PermissionContext {
    /// Boundary form in the baseline's `createDefaultPermissionContext` key
    /// order. Optional members are omitted exactly when the baseline omits
    /// them (`policyCanPrompt === false`, `acceptEdits !== undefined`, and a
    /// truthy `planDirectoryPath`).
    pub fn to_value(&self) -> JsValue {
        let mut value = JsObject::new();
        value.insert(
            "mode".into(),
            JsValue::String(self.mode.as_js_string().clone()),
        );
        value.insert("canPrompt".into(), JsValue::Bool(self.can_prompt));
        if self.policy_can_prompt == Some(false) {
            value.insert("policyCanPrompt".into(), JsValue::Bool(false));
        }
        value.insert(
            "bypassAvailable".into(),
            JsValue::Bool(self.bypass_available),
        );
        if let Some(accept_edits) = self.accept_edits {
            value.insert("acceptEdits".into(), JsValue::Bool(accept_edits));
        }
        value.insert("cwd".into(), JsValue::String(self.cwd.clone()));
        value.insert(
            "additionalWorkingDirectories".into(),
            JsValue::Array(
                self.additional_working_directories
                    .iter()
                    .cloned()
                    .map(JsValue::String)
                    .collect(),
            ),
        );
        if let Some(plan) = self
            .plan_directory_path
            .as_ref()
            .filter(|path| !path.is_empty())
        {
            value.insert("planDirectoryPath".into(), JsValue::String(plan.clone()));
        }
        value.insert("rules".into(), self.rules.to_value());
        JsValue::Object(value)
    }
}

/// Options for `createDefaultPermissionContext`. The baseline fills every
/// omitted option with a default, so all members are optional here.
#[derive(Clone, Debug, Default)]
pub struct DefaultPermissionContextOptions {
    pub cwd: JsString,
    pub mode: Option<PermissionMode>,
    pub can_prompt: Option<bool>,
    pub policy_can_prompt: Option<bool>,
    pub bypass_available: Option<bool>,
    pub accept_edits: Option<bool>,
    pub additional_working_directories: Option<Vec<JsString>>,
    pub plan_directory_path: Option<JsString>,
    /// `Partial<PermissionRuleSet>`: missing members are empty arrays.
    pub rules: PermissionRuleSet,
}

/// `createDefaultPermissionContext`.
pub fn create_default_permission_context(
    options: DefaultPermissionContextOptions,
) -> PermissionContext {
    PermissionContext {
        mode: options
            .mode
            .unwrap_or_else(|| PermissionMode::from(DEFAULT_PERMISSION_MODE)),
        can_prompt: options.can_prompt.unwrap_or(false),
        policy_can_prompt: (options.policy_can_prompt == Some(false)).then_some(false),
        bypass_available: options.bypass_available.unwrap_or(false),
        accept_edits: options.accept_edits,
        cwd: options.cwd,
        additional_working_directories: options.additional_working_directories.unwrap_or_default(),
        plan_directory_path: options.plan_directory_path,
        rules: options.rules,
    }
}

/// The baseline reads `tool.name`, `tool.kind`, `isReadOnly(input)` and the
/// optional async `checkPermissions(input, context)`. The full tool runtime
/// context is supplied by the adapter that owns it; the decision engine only
/// forwards the permission context it was given.
/// Borrowed counterpart of the baseline's async `checkPermissions`: the tool
/// receives the same input/context the decision is using, without cloning the
/// rule set on every call.
pub type PermissionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HostError>> + Send + 'a>>;

pub trait PermissionTool: Send + Sync {
    fn name(&self) -> &JsString;
    fn kind(&self) -> Option<&JsString> {
        None
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError>;
    fn check_permissions<'a>(
        &'a self,
        _input: &'a JsValue,
        _context: &'a PermissionContext,
    ) -> PermissionFuture<'a, Option<JsValue>> {
        Box::pin(async { Ok(None) })
    }
}

/// `matchPermissionRule`. `process_cwd` is the caller's already-resolved
/// `process.cwd()`; resolving it here would let each helper silently fall back
/// when the process cwd is gone, which is what #335 ② fixed at `decide`.
pub fn match_permission_rule(
    rule: &PermissionRule,
    tool_name: &JsString,
    input: &JsValue,
    context: Option<&PermissionContext>,
    process_cwd: &JsString,
) -> bool {
    if !matches_tool_name(&rule.tool_name, tool_name) {
        return false;
    }
    let pattern = rule.pattern.as_ref().filter(|pattern| !pattern.is_empty());
    if FILE_WRITE_TOOLS
        .iter()
        .any(|name| tool_name.equals_str(name))
        && pattern.is_none()
    {
        return is_file_input_inside_workspace(input, context, process_cwd);
    }
    match pattern {
        Some(pattern) => match_rule_pattern(pattern, tool_name, input, context, process_cwd),
        None => true,
    }
}

/// `PermissionRuntime.decide`. Returns the baseline decision object
/// (`allow`/`deny`/`ask`/`cancel`) in its original key order.
pub async fn decide(
    context: &PermissionContext,
    tool: &dyn PermissionTool,
    input: &JsValue,
    tool_call_id: &JsString,
) -> Result<JsValue, HostError> {
    let process_cwd = process_cwd_units()?;
    let tool_name = tool.name().clone();
    let session_allow_rule = find_matching_rule(
        context
            .rules
            .allow
            .iter()
            .filter(|rule| rule.source.equals_str("session")),
        &tool_name,
        input,
        context,
        &process_cwd,
    );
    let deny_rule = find_matching_rule(
        context.rules.deny.iter(),
        &tool_name,
        input,
        context,
        &process_cwd,
    );
    if let Some(deny_rule) = deny_rule {
        if let Some(session_allow_rule) = session_allow_rule
            && deny_rule.source.equals_str("user")
        {
            return allow_session_rule(context, tool, input, tool_call_id, session_allow_rule)
                .await;
        }
        return Ok(deny_from_rule(deny_rule));
    }

    let ask_rule = find_matching_rule(
        context.rules.ask.iter(),
        &tool_name,
        input,
        context,
        &process_cwd,
    );
    if let Some(ask_rule) = ask_rule {
        return finalize_ask(
            ask_from_rule(&tool_name, input, tool_call_id, ask_rule),
            context,
        );
    }

    if let Some(rule) = session_allow_rule {
        return allow_session_rule(context, tool, input, tool_call_id, rule).await;
    }

    if context.accept_edits == Some(true)
        && is_safe_workspace_edit(tool, input, context, &process_cwd)
    {
        return Ok(allow(reason_mode(
            &context.mode,
            concat(&[
                &literal("SDK acceptEdits allows workspace file tool "),
                &tool_name,
                &literal("."),
            ]),
        )));
    }

    if let Some(allow_rule) = find_matching_rule(
        context.rules.allow.iter(),
        &tool_name,
        input,
        context,
        &process_cwd,
    ) {
        if is_mode(&context.mode, "plan")
            && !truthy(&tool.is_read_only(input)?)
            && !is_plan_directory_write(tool, input, context, &process_cwd)
        {
            // Plan mode deny takes precedence over user allow rules below.
        } else {
            return Ok(allow(reason_rule(
                "allow",
                allow_rule,
                concat(&[&literal("Allow rule permits "), &tool_name, &literal(".")]),
            )));
        }
    }

    let tool_permission = tool.check_permissions(input, context).await?;
    let tool_decision = normalize_tool_permission(
        tool_permission,
        &tool_name,
        input,
        tool_call_id,
        &context.mode,
    );
    if let Some(tool_decision) = tool_decision {
        if type_is(&tool_decision, "ask") {
            if is_mode(&context.mode, "bypassPermissions") {
                return Ok(allow(reason_mode(
                    &context.mode,
                    concat(&[
                        &literal("Permission mode "),
                        context.mode.as_js_string(),
                        &literal(" overrides "),
                        &tool_name,
                        &literal(".checkPermissions ask."),
                    ]),
                )));
            }
            if is_mode(&context.mode, "plan") && truthy(&tool.is_read_only(input)?) {
                return Ok(allow(reason_mode(
                    &PermissionMode::from("plan"),
                    concat(&[
                        &literal("Plan mode allows read-only tool "),
                        &tool_name,
                        &literal(" despite .checkPermissions ask."),
                    ]),
                )));
            }
            if is_mode(&context.mode, "plan")
                && !is_plan_directory_write(tool, input, context, &process_cwd)
            {
                let message = plan_mode_deny_message(&tool_name, input);
                return Ok(deny(
                    reason_mode(&PermissionMode::from("plan"), message.clone()),
                    message,
                ));
            }
            return finalize_ask(tool_decision, context);
        }
        return Ok(tool_decision);
    }

    if is_mode(&context.mode, "bypassPermissions") {
        return Ok(allow(reason_mode(
            &context.mode,
            concat(&[
                &literal("Permission mode "),
                context.mode.as_js_string(),
                &literal(" allows "),
                &tool_name,
                &literal("."),
            ]),
        )));
    }

    let mode_decision = decide_by_mode(tool, input, tool_call_id, context, &process_cwd)?;
    if type_is(&mode_decision, "ask") {
        finalize_ask(mode_decision, context)
    } else {
        Ok(mode_decision)
    }
}

async fn allow_session_rule(
    context: &PermissionContext,
    tool: &dyn PermissionTool,
    input: &JsValue,
    tool_call_id: &JsString,
    rule: &PermissionRule,
) -> Result<JsValue, HostError> {
    let tool_permission = tool.check_permissions(input, context).await?;
    let tool_decision = normalize_tool_permission(
        tool_permission,
        tool.name(),
        input,
        tool_call_id,
        &context.mode,
    );
    if let Some(tool_decision) = tool_decision
        && !type_is(&tool_decision, "ask")
    {
        return Ok(tool_decision);
    }
    Ok(allow(reason_rule(
        "allow",
        rule,
        concat(&[
            &literal("Session allow rule permits "),
            tool.name(),
            &literal("."),
        ]),
    )))
}

fn decide_by_mode(
    tool: &dyn PermissionTool,
    input: &JsValue,
    tool_call_id: &JsString,
    context: &PermissionContext,
    process_cwd: &JsString,
) -> Result<JsValue, HostError> {
    let tool_name = tool.name();
    if is_mode(&context.mode, "plan") {
        if truthy(&tool.is_read_only(input)?) {
            return Ok(allow(reason_mode(
                &PermissionMode::from("plan"),
                concat(&[
                    &literal("Plan mode allows read-only tool "),
                    tool_name,
                    &literal("."),
                ]),
            )));
        }
        if is_plan_directory_write(tool, input, context, process_cwd) {
            return Ok(allow(reason_mode(
                &PermissionMode::from("plan"),
                literal("Plan mode allows writing markdown plans under the plan directory."),
            )));
        }
        let message = plan_mode_deny_message(tool_name, input);
        return Ok(deny(
            reason_mode(&PermissionMode::from("plan"), message.clone()),
            message,
        ));
    }

    if truthy(&tool.is_read_only(input)?) {
        return Ok(allow(reason_mode(
            &context.mode,
            concat(&[
                &literal("Mode "),
                context.mode.as_js_string(),
                &literal(" allows read-only tool "),
                tool_name,
                &literal("."),
            ]),
        )));
    }

    Ok(ask(
        tool_name,
        input,
        tool_call_id,
        reason_mode(
            &context.mode,
            concat(&[
                &literal("Mode "),
                context.mode.as_js_string(),
                &literal(" requires permission for "),
                tool_name,
                &literal("."),
            ]),
        ),
    ))
}

fn find_matching_rule<'a, I>(
    rules: I,
    tool_name: &JsString,
    input: &JsValue,
    context: &PermissionContext,
    process_cwd: &JsString,
) -> Option<&'a PermissionRule>
where
    I: IntoIterator<Item = &'a PermissionRule>,
{
    rules
        .into_iter()
        .find(|rule| match_permission_rule(rule, tool_name, input, Some(context), process_cwd))
}

fn finalize_ask(decision: JsValue, context: &PermissionContext) -> Result<JsValue, HostError> {
    if context.policy_can_prompt == Some(false) {
        return Ok(deny(
            reason_runtime(literal(
                "Permission prompt denied because Gateway policy disables prompts for this session.",
            )),
            literal(
                "Permission prompt denied because Gateway policy disables prompts for this session.",
            ),
        ));
    }

    // The baseline dereferences `decision.reason.type` unconditionally, so a
    // tool result that returns `ask` without a reason throws a TypeError. Keep
    // that fail-closed boundary instead of treating the malformed ask as an
    // ordinary prompt (which would let it reach the user and possibly allow).
    let reason = match js_get(&decision, "reason") {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => {
            return Err(malformed_ask("reason"));
        }
        Some(reason) => reason,
    };
    let reason_type = js_get(&reason, "type").and_then(|kind| kind.as_string().cloned());
    if reason_type
        .as_ref()
        .is_some_and(|kind| kind.equals_str("rule"))
    {
        // `decision.reason.rule.source` throws for a null/undefined rule, and
        // the force branch below throws for a non-string `rule.toolName`.
        let rule = match js_get(&reason, "rule") {
            None | Some(JsValue::Undefined) | Some(JsValue::Null) => {
                return Err(malformed_ask("reason.rule"));
            }
            Some(rule) => rule,
        };
        let source = js_get(&rule, "source");
        // A policy ask rule is evaluated before any remembered session allow
        // and stays an ask even under bypassPermissions.
        if source
            .as_ref()
            .and_then(JsValue::as_string)
            .is_some_and(|source| source.equals_str("policy"))
        {
            if !context.can_prompt {
                return Ok(deny(
                    reason_runtime(literal(
                        "Permission prompt denied because prompts are disabled for this session.",
                    )),
                    literal(
                        "Permission prompt denied because prompts are disabled for this session.",
                    ),
                ));
            }
            return Ok(decision);
        }

        let force = js_get(&rule, "force").and_then(|force| force.as_bool());
        if force == Some(true)
            && source
                .as_ref()
                .and_then(JsValue::as_string)
                .is_some_and(|source| source.equals_str("session"))
        {
            let Some(name) = js_get(&rule, "toolName").and_then(|name| name.as_string().cloned())
            else {
                return Err(malformed_ask("reason.rule.toolName"));
            };
            if starts_with(name.units(), &mcp_prefix()) {
                return Ok(decision);
            }
        }
    }

    if is_mode(&context.mode, "bypassPermissions") {
        return Ok(allow(reason_mode(
            &context.mode,
            literal("bypassPermissions mode skips permission prompts."),
        )));
    }

    if !context.can_prompt {
        return Ok(deny(
            reason_runtime(literal(
                "Permission prompt denied because prompts are disabled for this session.",
            )),
            literal("Permission prompt denied because prompts are disabled for this session."),
        ));
    }

    Ok(decision)
}

fn malformed_ask(path: &str) -> HostError {
    HostError::new(
        HostErrorKind::Protocol,
        format!(
            "permission decision is malformed: ask is missing {path} (the TypeScript baseline throws)"
        ),
    )
}

fn normalize_tool_permission(
    result: Option<JsValue>,
    tool_name: &JsString,
    input: &JsValue,
    tool_call_id: &JsString,
    mode: &PermissionMode,
) -> Option<JsValue> {
    let result = result?;
    if !truthy(&result) || type_is(&result, "passthrough") {
        return None;
    }

    if type_is(&result, "ask") {
        let mut decision = spread(&result);
        let mut request = js_get(&result, "request")
            .map(|request| spread(&request))
            .unwrap_or_default();
        request.insert("toolCallId".into(), JsValue::String(tool_call_id.clone()));
        request.insert("toolName".into(), JsValue::String(tool_name.clone()));
        decision.insert("request".into(), JsValue::Object(request));
        return Some(JsValue::Object(decision));
    }

    if type_is(&result, "allow") || type_is(&result, "deny") || type_is(&result, "cancel") {
        return Some(result);
    }

    Some(ask(
        tool_name,
        input,
        tool_call_id,
        reason_runtime(concat(&[
            &literal("Permission result for "),
            tool_name,
            &literal(" was not recognized in mode "),
            mode.as_js_string(),
            &literal("."),
        ])),
    ))
}

fn allow(reason: JsValue) -> JsValue {
    let mut decision = JsObject::new();
    decision.insert("type".into(), JsValue::from("allow"));
    decision.insert("reason".into(), reason);
    JsValue::Object(decision)
}

fn deny(reason: JsValue, message: JsString) -> JsValue {
    let mut decision = JsObject::new();
    decision.insert("type".into(), JsValue::from("deny"));
    decision.insert("reason".into(), reason);
    decision.insert("message".into(), JsValue::String(message));
    JsValue::Object(decision)
}

fn deny_from_rule(rule: &PermissionRule) -> JsValue {
    deny(
        reason_rule(
            "deny",
            rule,
            concat(&[
                &literal("Deny rule blocks "),
                &rule.tool_name,
                &literal("."),
            ]),
        ),
        concat(&[
            &literal("Deny rule blocks "),
            &rule.tool_name,
            &literal("."),
        ]),
    )
}

fn ask_from_rule(
    tool_name: &JsString,
    input: &JsValue,
    tool_call_id: &JsString,
    rule: &PermissionRule,
) -> JsValue {
    ask(
        tool_name,
        input,
        tool_call_id,
        reason_rule(
            "ask",
            rule,
            concat(&[
                &literal("Ask rule requires confirmation for "),
                tool_name,
                &literal("."),
            ]),
        ),
    )
}

fn ask(tool_name: &JsString, input: &JsValue, tool_call_id: &JsString, reason: JsValue) -> JsValue {
    let request = create_permission_request(tool_name, input, tool_call_id, reason.clone());
    let mut decision = JsObject::new();
    decision.insert("type".into(), JsValue::from("ask"));
    decision.insert("reason".into(), reason);
    decision.insert("request".into(), JsValue::Object(request));
    JsValue::Object(decision)
}

fn create_permission_request(
    tool_name: &JsString,
    input: &JsValue,
    tool_call_id: &JsString,
    reason: JsValue,
) -> JsObject {
    let mut request = JsObject::new();
    request.insert("toolCallId".into(), JsValue::String(tool_call_id.clone()));
    request.insert("toolName".into(), JsValue::String(tool_name.clone()));
    request.insert(
        "inputSummary".into(),
        JsValue::String(summarize_input(input)),
    );
    request.insert("reason".into(), reason);
    request.insert(
        "options".into(),
        JsValue::Array(vec![
            permission_option("allow_once", "Allow once"),
            permission_option("deny", "Deny"),
            permission_option("cancel", "Cancel"),
        ]),
    );
    request
}

fn permission_option(id: &str, label: &str) -> JsValue {
    let mut option = JsObject::new();
    option.insert("id".into(), JsValue::from(id));
    option.insert("label".into(), JsValue::from(label));
    JsValue::Object(option)
}

fn reason_mode(mode: &PermissionMode, message: JsString) -> JsValue {
    let mut reason = JsObject::new();
    reason.insert("type".into(), JsValue::from("mode"));
    reason.insert("mode".into(), JsValue::String(mode.as_js_string().clone()));
    reason.insert("message".into(), JsValue::String(message));
    JsValue::Object(reason)
}

fn reason_rule(behavior: &str, rule: &PermissionRule, message: JsString) -> JsValue {
    let mut reason = JsObject::new();
    reason.insert("type".into(), JsValue::from("rule"));
    reason.insert("behavior".into(), JsValue::from(behavior));
    reason.insert("rule".into(), rule.to_value());
    reason.insert("message".into(), JsValue::String(message));
    JsValue::Object(reason)
}

fn reason_runtime(message: JsString) -> JsValue {
    let mut reason = JsObject::new();
    reason.insert("type".into(), JsValue::from("runtime"));
    reason.insert("message".into(), JsValue::String(message));
    JsValue::Object(reason)
}

/// `summarizeInput`: `JSON.stringify` when it produces a string, otherwise
/// `String(input)`, with the first 500 UTF-16 units and a trailing `...`.
fn summarize_input(input: &JsValue) -> JsString {
    match pilotdeck_protocol::stringify_json(input) {
        Ok(Some(json)) => truncate_summary(&json.encode_utf16().collect::<Vec<_>>()),
        Ok(None) => match js_value::to_string_units(input) {
            Ok(units) => truncate_summary(&units),
            Err(_) => literal("[unserializable input]"),
        },
        Err(_) => literal("[unserializable input]"),
    }
}

fn truncate_summary(units: &[u16]) -> JsString {
    if units.len() > 500 {
        let mut truncated = units[..500].to_vec();
        truncated.extend_from_slice(literal("...").units());
        JsString::from_utf16(truncated)
    } else {
        JsString::from_utf16(units.to_vec())
    }
}

fn is_mode(mode: &PermissionMode, expected: &str) -> bool {
    mode.as_js_string().equals_str(expected)
}

fn type_is(value: &JsValue, expected: &str) -> bool {
    js_get(value, "type")
        .and_then(|kind| kind.as_string().cloned())
        .is_some_and(|kind| kind.equals_str(expected))
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

/// Property read for the value domains the decision engine inspects. Explicit
/// prototypes are honoured, matching previous permission-adjacent ports.
fn js_get(value: &JsValue, name: &str) -> Option<JsValue> {
    match value {
        JsValue::Object(object) => object.get_property(name).cloned(),
        JsValue::Array(items) => {
            let key = JsString::from(name);
            if name == "length" {
                return Some(JsValue::Number(items.len() as f64));
            }
            key.array_index()
                .and_then(|index| items.get(index as usize).cloned())
        }
        JsValue::String(string) => {
            if name == "length" {
                return Some(JsValue::Number(string.len_utf16() as f64));
            }
            JsString::from(name)
                .array_index()
                .and_then(|index| string.units().get(index as usize))
                .map(|unit| JsValue::String(JsString::from_utf16(vec![*unit])))
        }
        _ => None,
    }
}

/// Object spread (`{...value}`) for the wire value domain. Only own
/// enumerable members are copied; an explicit prototype is not part of the
/// spread result.
fn spread(value: &JsValue) -> JsObject {
    match value {
        JsValue::Object(object) => {
            let mut spread = JsObject::new();
            for (key, value) in object.entries() {
                spread.insert(key.clone(), value.clone());
            }
            spread
        }
        JsValue::Array(items) => {
            let mut object = JsObject::new();
            for (index, value) in items.iter().enumerate() {
                object.insert(index.to_string().into(), value.clone());
            }
            object
        }
        JsValue::String(string) => {
            let mut object = JsObject::new();
            for (index, unit) in string.units().iter().enumerate() {
                object.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
            object
        }
        _ => JsObject::new(),
    }
}

const FILE_WRITE_TOOLS: [&str; 2] = ["write_file", "edit_file"];
const FILE_PATH_PATTERN_TOOLS: [&str; 4] =
    ["read_file", "send_attachment", "write_file", "edit_file"];

fn matches_tool_name(rule_tool_name: &JsString, tool_name: &JsString) -> bool {
    if rule_tool_name == tool_name {
        return true;
    }
    contains(rule_tool_name.units(), STAR)
        && wildcard_match(rule_tool_name.units(), tool_name.units())
}

fn match_rule_pattern(
    pattern: &JsString,
    tool_name: &JsString,
    input: &JsValue,
    context: Option<&PermissionContext>,
    process_cwd: &JsString,
) -> bool {
    if pattern.is_empty() {
        return true;
    }
    if tool_name.equals_str("bash") {
        return match_bash_pattern(pattern, input);
    }
    if FILE_PATH_PATTERN_TOOLS
        .iter()
        .any(|name| tool_name.equals_str(name))
    {
        return match_file_path_pattern(pattern, input, context, process_cwd);
    }
    true
}

fn match_bash_pattern(pattern: &JsString, input: &JsValue) -> bool {
    let Some(command) = read_command(input) else {
        return false;
    };
    let mut pattern_units = pattern.units().to_vec();
    if pattern_units.len() >= 2
        && pattern_units[pattern_units.len() - 1] == STAR
        && pattern_units[pattern_units.len() - 2] == COLON
    {
        pattern_units.truncate(pattern_units.len() - 2);
        pattern_units.push(STAR);
    }
    wildcard_match(&pattern_units, command.units())
}

fn read_command(input: &JsValue) -> Option<JsString> {
    if !object_like(input) {
        return None;
    }
    let command = js_get(input, "command")?.as_string()?.clone();
    let trimmed = trim_units(command.units());
    (!trimmed.is_empty()).then(|| JsString::from_utf16(trimmed.to_vec()))
}

fn match_file_path_pattern(
    pattern: &JsString,
    input: &JsValue,
    context: Option<&PermissionContext>,
    process_cwd: &JsString,
) -> bool {
    let Some(path) = resolve_input_file_path(input, context, process_cwd) else {
        return false;
    };
    wildcard_match(
        &normalize_backslashes(pattern.units()),
        &normalize_backslashes(path.units()),
    )
}

fn is_file_input_inside_workspace(
    input: &JsValue,
    context: Option<&PermissionContext>,
    process_cwd: &JsString,
) -> bool {
    let Some(context) = context else {
        return false;
    };
    let Some(path) = resolve_input_file_path(input, Some(context), process_cwd) else {
        return false;
    };
    let mut roots = vec![resolve_one(process_cwd, context.cwd.units())];
    roots.extend(
        context
            .additional_working_directories
            .iter()
            .map(|root| resolve_one(process_cwd, root.units())),
    );
    roots
        .iter()
        .any(|root| is_path_within_root(&path, root, process_cwd))
}

fn resolve_input_file_path(
    input: &JsValue,
    context: Option<&PermissionContext>,
    process_cwd: &JsString,
) -> Option<JsString> {
    let context = context?;
    let file_path = read_file_path(input)?;
    if file_path.units().contains(&0) {
        return None;
    }
    Some(resolve_input(process_cwd, &context.cwd, &file_path))
}

fn read_file_path(input: &JsValue) -> Option<JsString> {
    if !object_like(input) {
        return None;
    }
    let file_path = js_get(input, "file_path");
    let value = match file_path {
        Some(value) if !matches!(value, JsValue::Undefined | JsValue::Null) => Some(value),
        _ => js_get(input, "filePath"),
    }?;
    let file_path = value.as_string()?;
    let trimmed = trim_units(file_path.units());
    (!trimmed.is_empty()).then(|| JsString::from_utf16(trimmed.to_vec()))
}

fn object_like(value: &JsValue) -> bool {
    matches!(value, JsValue::Object(_) | JsValue::Array(_))
}

fn is_safe_workspace_edit(
    tool: &dyn PermissionTool,
    input: &JsValue,
    context: &PermissionContext,
    process_cwd: &JsString,
) -> bool {
    let name = tool.name();
    if !(name.equals_str("write_file")
        || name.equals_str("edit_file")
        || name.equals_str("edit_notebook"))
    {
        return false;
    }
    if !object_like(input) {
        return false;
    }
    let path = js_get(
        input,
        if name.equals_str("edit_notebook") {
            "notebook_path"
        } else {
            "file_path"
        },
    );
    let Some(path) = path.as_ref().and_then(JsValue::as_string) else {
        return false;
    };
    if trim_units(path.units()).is_empty() {
        return false;
    }
    workspace_path_ok(path, context, process_cwd)
}

/// The `forWrite: true` subset of `resolvePilotDeckWorkspacePath`: no
/// `mustExist`, registered-read-file or outside-workspace allowances apply.
fn workspace_path_ok(
    input_path: &JsString,
    context: &PermissionContext,
    process_cwd: &JsString,
) -> bool {
    if input_path.is_empty() || input_path.units().contains(&0) {
        return false;
    }
    let absolute = resolve_input(process_cwd, &context.cwd, input_path);
    if is_mode(&context.mode, "bypassPermissions") {
        let relative = relative_or_dot(&context.cwd, &absolute, process_cwd);
        return !is_write_denied(&relative);
    }
    let mut roots = vec![resolve_one(process_cwd, context.cwd.units())];
    roots.extend(
        context
            .additional_working_directories
            .iter()
            .map(|root| resolve_one(process_cwd, root.units())),
    );
    let Some(root) = roots
        .into_iter()
        .find(|root| is_path_within_root(&absolute, root, process_cwd))
    else {
        return false;
    };
    !is_write_denied(&relative_or_dot(&root, &absolute, process_cwd))
}

fn relative_or_dot(from: &JsString, to: &JsString, process_cwd: &JsString) -> JsString {
    let relative = relative_path(from, to, process_cwd);
    if relative.is_empty() {
        literal(".")
    } else {
        relative
    }
}

fn is_write_denied(relative_path: &JsString) -> bool {
    let first = relative_path
        .units()
        .split(|unit| *unit == SLASH)
        .next()
        .unwrap_or_default();
    [".git", "node_modules", "dist"]
        .iter()
        .any(|name| first.iter().copied().eq(name.encode_utf16()))
}

fn is_plan_directory_write(
    tool: &dyn PermissionTool,
    input: &JsValue,
    context: &PermissionContext,
    process_cwd: &JsString,
) -> bool {
    if !tool
        .kind()
        .is_some_and(|kind| kind.equals_str("filesystem"))
    {
        return false;
    }
    let Some(plan_directory) = context
        .plan_directory_path
        .as_ref()
        .filter(|path| !path.is_empty())
    else {
        return false;
    };
    if !object_like(input) {
        return false;
    }
    let file_path = js_get(input, "file_path");
    let value = match file_path {
        Some(value) if !matches!(value, JsValue::Undefined | JsValue::Null) => Some(value),
        _ => js_get(input, "filePath"),
    };
    let Some(file_path) = value.as_ref().and_then(JsValue::as_string) else {
        return false;
    };
    let absolute = resolve_input(process_cwd, &context.cwd, file_path);
    if !ends_with_markdown(&absolute) {
        return false;
    }
    let relative = relative_path(plan_directory, &absolute, process_cwd);
    !relative.is_empty()
        && !is_absolute_units(relative.units())
        && !starts_with(relative.units(), &[DOT, DOT])
        && !starts_with(relative.units(), &[DOT, DOT, SLASH])
}

fn ends_with_markdown(path: &JsString) -> bool {
    let units = path.units();
    units.len() >= 3
        && units[units.len() - 3] == DOT
        && matches!(units[units.len() - 2], 0x006d | 0x004d)
        && matches!(units[units.len() - 1], 0x0064 | 0x0044)
}

fn plan_mode_deny_message(tool_name: &JsString, input: &JsValue) -> JsString {
    if tool_name.equals_str("bash") {
        let command = js_get(input, "command")
            .and_then(|command| command.as_string().cloned())
            .unwrap_or_default();
        let truncated = if command.len_utf16() > 120 {
            concat(&[
                &JsString::from_utf16(command.units()[..120].to_vec()),
                &literal("…"),
            ])
        } else {
            command
        };
        return concat(&[
            &literal("[PLAN_MODE_VIOLATION] bash command \""),
            &truncated,
            &literal(
                "\" is BLOCKED — write/modify commands are not allowed in plan mode.\n\nIn plan mode, bash is restricted to READ-ONLY commands only (ls, cat, git status, git log, git diff, pwd, find, head, wc, etc.).\n\nRewrite your command as a read-only operation, or use read_file/grep/glob instead.",
            ),
        ]);
    }
    concat(&[
        &literal("[PLAN_MODE_VIOLATION] Tool \""),
        tool_name,
        &literal(
            "\" is BLOCKED in plan mode.\n\nYou are in READ-ONLY plan mode. This tool cannot be executed.\n\nWhat you should do instead:\n1. Use read-only tools (read_file, grep, glob, bash with read-only commands) to explore\n2. Write your plan as markdown under .pilotdeck/plans/\n3. Call exit_plan_mode when your plan is ready\n\nDo NOT retry this tool. It will fail again.",
        ),
    ])
}

// ---- JS string, path and wildcard primitives ------------------------------

const SLASH: u16 = b'/' as u16;
const DOT: u16 = b'.' as u16;
const COLON: u16 = b':' as u16;
const STAR: u16 = b'*' as u16;

fn literal(text: &str) -> JsString {
    JsString::from(text)
}

fn concat(parts: &[&JsString]) -> JsString {
    let mut units = Vec::new();
    for part in parts {
        units.extend_from_slice(part.units());
    }
    JsString::from_utf16(units)
}

fn contains(units: &[u16], needle: u16) -> bool {
    units.contains(&needle)
}

fn starts_with(units: &[u16], prefix: &[u16]) -> bool {
    units.starts_with(prefix)
}

fn mcp_prefix() -> [u16; 5] {
    let mut prefix = [0; 5];
    for (index, unit) in "mcp__".encode_utf16().enumerate() {
        prefix[index] = unit;
    }
    prefix
}

fn is_js_space(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

/// `String.prototype.trim` on UTF-16 units (all whitespace is BMP).
fn trim_units(units: &[u16]) -> &[u16] {
    let start = units
        .iter()
        .position(|unit| !is_js_space(*unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !is_js_space(*unit))
        .map_or(start, |index| index + 1);
    &units[start..end]
}

fn normalize_backslashes(units: &[u16]) -> Vec<u16> {
    units
        .iter()
        .map(|unit| {
            if *unit == u16::from(b'\\') {
                SLASH
            } else {
                *unit
            }
        })
        .collect()
}

fn is_line_terminator(unit: u16) -> bool {
    matches!(unit, 0x000a | 0x000d | 0x2028 | 0x2029)
}

/// `wildcardToRegExp` for the subset the baseline can produce: every regex
/// metacharacter is escaped, so the pattern is literal chunks separated by
/// `*`; each `*` becomes `.*`, which stops at a JS line terminator.
fn wildcard_match(pattern: &[u16], text: &[u16]) -> bool {
    let (mut pattern_index, mut text_index) = (0, 0);
    let mut star = None;
    let mut star_text = 0;
    while text_index < text.len() {
        if pattern_index < pattern.len()
            && pattern[pattern_index] != STAR
            && pattern[pattern_index] == text[text_index]
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == STAR {
            star = Some(pattern_index);
            star_text = text_index;
            pattern_index += 1;
        } else if let Some(star_index) = star {
            if star_text >= text.len() || is_line_terminator(text[star_text]) {
                return false;
            }
            star_text += 1;
            text_index = star_text;
            pattern_index = star_index + 1;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == STAR {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn is_absolute_units(value: &[u16]) -> bool {
    value.first() == Some(&SLASH)
}

/// Lexical `node:path.normalize` on POSIX (`/` separator, no filesystem).
fn normalize_path(value: &[u16]) -> Vec<u16> {
    let absolute = is_absolute_units(value);
    let mut segments: Vec<&[u16]> = Vec::new();
    for segment in value.split(|unit| *unit == SLASH) {
        if segment.is_empty() || (segment.len() == 1 && segment[0] == DOT) {
            continue;
        }
        if segment.len() == 2 && segment[0] == DOT && segment[1] == DOT {
            match segments.last() {
                Some(last) if !(last.len() == 2 && last[0] == DOT && last[1] == DOT) => {
                    segments.pop();
                }
                _ if !absolute => segments.push(segment),
                _ => {}
            }
            continue;
        }
        segments.push(segment);
    }
    let mut out = Vec::new();
    if absolute {
        out.push(SLASH);
    }
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            out.push(SLASH);
        }
        out.extend_from_slice(segment);
    }
    if !absolute && out.is_empty() {
        out.push(DOT);
    }
    out
}

/// `node:path.join(base, value)` on POSIX. `join` skips empty segments, so an
/// empty base contributes nothing: `join("", "x")` is `"x"`, not `"/x"`. The
/// caller resolves the result against the process cwd (as the baseline does),
/// which is what makes `cwd: ""` mean "the process cwd" rather than root.
fn join_normalized(base: &[u16], value: &[u16]) -> Vec<u16> {
    if base.is_empty() {
        return normalize_path(value);
    }
    let mut combined = Vec::with_capacity(base.len() + 1 + value.len());
    combined.extend_from_slice(base);
    combined.push(SLASH);
    combined.extend_from_slice(value);
    normalize_path(&combined)
}

/// `node:path.resolve(process.cwd(), value)` for one already-joined value.
fn resolve_one(process_cwd: &JsString, value: &[u16]) -> JsString {
    let normalized = normalize_path(value);
    if is_absolute_units(&normalized) {
        JsString::from_utf16(normalized)
    } else {
        JsString::from_utf16(join_normalized(process_cwd.units(), &normalized))
    }
}

fn resolve_input(process_cwd: &JsString, cwd: &JsString, value: &JsString) -> JsString {
    let joined = if is_absolute_units(value.units()) {
        normalize_path(value.units())
    } else {
        join_normalized(cwd.units(), value.units())
    };
    resolve_one(process_cwd, &joined)
}

fn path_segments(value: &[u16]) -> Vec<&[u16]> {
    value
        .split(|unit| *unit == SLASH)
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// `node:path.relative(from, to)` on POSIX, resolving both against process cwd.
fn relative_path(from: &JsString, to: &JsString, process_cwd: &JsString) -> JsString {
    let from = resolve_one(process_cwd, from.units());
    let to = resolve_one(process_cwd, to.units());
    if from == to {
        return JsString::default();
    }
    let from_segments = path_segments(from.units());
    let to_segments = path_segments(to.units());
    let common = from_segments
        .iter()
        .zip(&to_segments)
        .take_while(|(left, right)| left == right)
        .count();
    let mut units = Vec::new();
    for _ in common..from_segments.len() {
        if !units.is_empty() {
            units.push(SLASH);
        }
        units.extend_from_slice(&[DOT, DOT]);
    }
    for segment in &to_segments[common..] {
        if !units.is_empty() {
            units.push(SLASH);
        }
        units.extend_from_slice(segment);
    }
    JsString::from_utf16(units)
}

fn is_path_within_root(candidate: &JsString, root: &JsString, process_cwd: &JsString) -> bool {
    let relative = relative_path(root, candidate, process_cwd);
    relative.is_empty()
        || (!is_absolute_units(relative.units()) && !starts_with(relative.units(), &[DOT, DOT]))
}

/// Node's `process.cwd()` on the cold path (`uv_cwd` real syscall). A missing
/// cwd throws `ENOENT ... uv_cwd`; TS never substitutes an empty string. The
/// warm path is Node's process-wide cache and is intentionally not modelled
/// here: failing closed on a deleted cwd is stricter than returning the stale
/// cached path (#335 ②).
fn process_cwd_units() -> Result<JsString, HostError> {
    std::env::current_dir()
        .map(|path| JsString::from(path.to_string_lossy().as_ref()))
        .map_err(|error| HostError::new(HostErrorKind::Io, process_cwd_error(&error)))
}

fn process_cwd_error(error: &std::io::Error) -> String {
    let detail = match error.raw_os_error() {
        Some(2) => "ENOENT: no such file or directory",
        Some(13) => "EACCES: permission denied",
        Some(1) => "EPERM: operation not permitted",
        _ => return error.to_string(),
    };
    format!("{detail}, uv_cwd")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(source: &str, behavior: &str, tool: &str, pattern: Option<&str>) -> PermissionRule {
        match pattern {
            Some(pattern) => PermissionRule::new(source, behavior, tool).with_pattern(pattern),
            None => PermissionRule::new(source, behavior, tool),
        }
    }

    fn context() -> PermissionContext {
        create_default_permission_context(DefaultPermissionContextOptions {
            cwd: "/ws/project".into(),
            ..Default::default()
        })
    }

    #[test]
    fn wildcard_matches_literal_chunks_and_case_sensitively() {
        assert!(wildcard_match(
            literal("read_*").units(),
            literal("read_file").units()
        ));
        assert!(!wildcard_match(
            literal("Read_*").units(),
            literal("read_file").units()
        ));
        assert!(wildcard_match(&[STAR], literal("anything").units()));
        assert!(wildcard_match(&[STAR], &[]));
        assert!(!wildcard_match(&[STAR], literal("a\nb").units()));
        assert!(wildcard_match(
            literal("a+b?").units(),
            literal("a+b?").units()
        ));
    }

    #[test]
    fn node_path_normalization_is_lexical_and_root_clamped() {
        let cwd = literal("/ws/project");
        assert_eq!(
            resolve_input(&cwd, &cwd, &literal("../extra//./a/.."))
                .to_utf8()
                .unwrap(),
            "/ws/extra"
        );
        assert_eq!(
            resolve_input(&cwd, &cwd, &literal("/../.."))
                .to_utf8()
                .unwrap(),
            "/"
        );
        assert_eq!(
            relative_path(&literal("/a/b"), &literal("/a/bc"), &cwd)
                .to_utf8()
                .unwrap(),
            "../bc"
        );
        assert!(relative_path(&literal("/a/b"), &literal("/a/b"), &cwd).is_empty());
    }

    #[test]
    fn trim_keeps_lone_surrogates_and_trims_js_whitespace() {
        let value = JsString::from_utf16(vec![0xfeff, 0xd800, 0x20, 0x3000]);
        let trimmed = trim_units(value.units());
        assert_eq!(trimmed, &[0xd800u16]);
    }

    #[test]
    fn write_and_edit_rules_without_pattern_require_workspace_membership() {
        let context = context();
        let process_cwd = literal("/ws/project");
        let input = JsValue::Object(object([("file_path", JsValue::from("src/a.rs"))]));
        assert!(match_permission_rule(
            &rule("user", "allow", "write_file", None),
            &literal("write_file"),
            &input,
            Some(&context),
            &process_cwd
        ));
        let outside = JsValue::Object(object([("file_path", JsValue::from("../a.rs"))]));
        assert!(!match_permission_rule(
            &rule("user", "allow", "write_file", None),
            &literal("write_file"),
            &outside,
            Some(&context),
            &process_cwd
        ));
        let read = JsValue::Object(object([("file_path", JsValue::from("../a.rs"))]));
        assert!(match_permission_rule(
            &rule("user", "allow", "read_file", None),
            &literal("read_file"),
            &read,
            Some(&context),
            &process_cwd
        ));
    }

    #[test]
    fn summarize_input_truncates_at_five_hundred_utf16_units() {
        let input = JsValue::String("x".repeat(600).into());
        let summary = summarize_input(&input);
        assert_eq!(summary.len_utf16(), 503);
        assert!(summary.units().ends_with(literal("...").units()));
        assert_eq!(
            summarize_input(&JsValue::Undefined).to_utf8().unwrap(),
            "undefined"
        );
    }

    #[test]
    fn spread_ignores_an_explicit_prototype() {
        let mut prototype = JsObject::new();
        prototype.insert("inherited".into(), JsValue::from("value"));
        let mut source = object([("own", JsValue::Bool(true))]);
        source.set_prototype(JsValue::Object(prototype));
        let spread = spread(&JsValue::Object(source));
        assert_eq!(spread.get("own"), Some(&JsValue::Bool(true)));
        assert!(spread.get("inherited").is_none());
        assert!(spread.prototype().is_none());
    }

    #[test]
    fn empty_cwd_is_skipped_like_node_path_join() {
        // Node: join("", "x") === "x", then resolve against process.cwd.
        assert_eq!(
            join_normalized(&[], literal("x").units()),
            literal("x").units()
        );
        assert_eq!(join_normalized(&[], &[]), vec![DOT]);
        let process_cwd = literal("/work");
        for (cwd, expected) in [
            ("", "/work/secret.env"),
            (".", "/work/secret.env"),
            ("rel/path", "/work/rel/path/secret.env"),
        ] {
            assert_eq!(
                resolve_input(&process_cwd, &literal(cwd), &literal("secret.env"))
                    .to_utf8()
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn node_path_probe_values_are_reproduced() {
        let process_cwd = literal("/work");
        for (input, expected) in [
            ("/a//b/./c/", "/a/b/c"),
            ("/a/../..", "/"),
            ("../c", "/c"),
            ("//b", "/b"),
        ] {
            assert_eq!(
                resolve_one(&process_cwd, literal(input).units())
                    .to_utf8()
                    .unwrap(),
                expected
            );
        }
        assert_eq!(
            relative_path(&literal("/a/b"), &literal("/a"), &process_cwd)
                .to_utf8()
                .unwrap(),
            ".."
        );
        assert_eq!(
            relative_path(&literal("/"), &literal("/a"), &process_cwd)
                .to_utf8()
                .unwrap(),
            "a"
        );
    }

    fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
        let mut object = JsObject::new();
        for (key, value) in entries {
            object.insert(key.into(), value);
        }
        object
    }
}
