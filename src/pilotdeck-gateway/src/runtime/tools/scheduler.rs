//! Whole-batch safe-first scheduling; results retain caller order.
use super::{
    registry::{self, ToolRegistry},
    *,
};
use crate::config::changes::{Operand, object_property, object_prototype_property};
use crate::host::{
    HostFuture,
    modules::{
        capability::{ToolCall, ToolCatalog, ToolDefinition, ToolExecution, ToolInvocationContext},
        permission::PermissionDecision,
        tool_context::ExecutionContext,
    },
};
use crate::js_value;
use std::{future::poll_fn, task::Poll};

struct GrantCleanup {
    permission: crate::runtime::project::interaction_permission::TurnPermission,
    tool: JsString,
    call: JsString,
    input: JsValue,
}
impl Drop for GrantCleanup {
    fn drop(&mut self) {
        let _ = self
            .permission
            .revoke_native_grant(&self.tool, &self.call, &self.input);
    }
}

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
    mode_owner: Option<Arc<dyn PermissionDecision>>,
    turn_permission:
        Arc<std::sync::OnceLock<crate::runtime::project::interaction_permission::TurnPermission>>,
    turn_hooks: Arc<std::sync::OnceLock<crate::runtime::project::interaction_hooks::TurnHooks>>,
    native_sandbox: Option<JsString>,
    #[cfg(unix)]
    native_sandbox_provider: Option<Arc<super::sandbox::NativeSandbox>>,
    pub mode: SchedulerMode,
}
impl ToolRuntime {
    async fn dispatch_tool_lifecycle(
        &self,
        event: &str,
        name: &JsString,
        call: &ToolCall,
        input: &JsValue,
        context: &ToolRunContext,
        extra: Option<JsObject>,
    ) -> Result<Option<crate::runtime::hooks::LifecycleDispatchResult>, HostError> {
        use crate::host::modules::lifecycle::LifecyclePort;
        use crate::runtime::hooks::LifecycleDispatchInput;
        let Some(hooks) = self.turn_hooks.get() else {
            return Ok(None);
        };
        let cwd = context
            .fields
            .get("cwd")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let base_input = object([
            (
                "sessionId",
                context
                    .fields
                    .get("sessionId")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            ),
            ("transcriptPath", "".into()),
            ("cwd", JsValue::String(cwd.clone())),
            (
                "permissionMode",
                context
                    .fields
                    .get("permissionMode")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            ),
        ]);
        let mut payload = object([
            ("toolName", JsValue::String(name.clone())),
            ("toolInput", input.clone()),
            ("toolUseId", JsValue::String(call.id.clone())),
        ]);
        if let Some(extra) = extra {
            for (key, value) in extra.entries() {
                payload.insert(key.clone(), value.clone());
            }
        }
        hooks
            .dispatch(LifecycleDispatchInput {
                event: event.into(),
                base_input,
                payload: Some(payload),
                match_query: Some(name.clone()),
                cwd,
                env: context
                    .fields
                    .get("env")
                    .and_then(JsValue::as_object)
                    .cloned(),
                signal: None,
            })
            .await
            .map(Some)
    }
    pub fn new(
        registry: ToolRegistry,
        permission: Arc<dyn PermissionDecision>,
        mode: SchedulerMode,
    ) -> Self {
        Self {
            registry,
            permission,
            mode_owner: None,
            turn_permission: Arc::default(),
            turn_hooks: Arc::default(),
            mode,
            native_sandbox: None,
            #[cfg(unix)]
            native_sandbox_provider: None,
        }
    }
    pub(crate) fn with_mode_owner(mut self, owner: Arc<dyn PermissionDecision>) -> Self {
        self.mode_owner = Some(owner);
        self
    }
    fn fresh_mode(&self, mut context: ToolRunContext) -> ToolRunContext {
        if let Some(mode) = self
            .mode_owner
            .as_ref()
            .and_then(|owner| owner.current_mode())
        {
            let mode = JsValue::String(mode.as_js_string().clone());
            context.fields.insert("permissionMode".into(), mode.clone());
            if let Some(mut permissions) = context
                .fields
                .get("permissionContext")
                .and_then(JsValue::as_object)
                .cloned()
            {
                permissions.insert("mode".into(), mode);
                context
                    .fields
                    .insert("permissionContext".into(), JsValue::Object(permissions));
            }
        }
        context
    }
    /// Explicit native fallback until an execution-world sandbox provider is
    /// installed. Never silently execute a restricted profile unrestricted.
    pub fn with_native_sandbox(mut self, mode: Option<JsString>) -> Self {
        self.native_sandbox = mode;
        self
    }

    #[cfg(unix)]
    pub fn with_native_sandbox_provider(
        mut self,
        provider: Arc<super::sandbox::NativeSandbox>,
    ) -> Self {
        self.native_sandbox_provider = Some(provider);
        self
    }

    pub fn execute_batch(
        &self,
        calls: Vec<ToolCall>,
        context: ToolRunContext,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<JsObject>, HostError>> + Send + '_>> {
        Box::pin(async move {
            let mut concurrent = Vec::new();
            let mut sequential = Vec::new();
            let mut transitions = Vec::new();
            // Predicates are evaluated for the entire batch before any call starts.
            // Single-call batches bypass the predicate, exactly like TS.
            for (index, call) in calls.iter().enumerate() {
                // Deliberate deviation (fixtures/tool-runtime/README.md): TS runs
                // every concurrencySafe tool in the concurrent phase; here only
                // those that are also read-only do, so a sibling error, which
                // drops the concurrent batch, never cuts a write short. Other safe
                // Ledger: specs/gateway-parity/accepted-diffs.md
                // A6.5-TOOLS-SCHEDULE-001 (unapproved). Other safe
                // tools (e.g. agent) run in the sequential phase. isReadOnly is
                // asked only of concurrencySafe tools; a throw counts as false.
                let tool = self.registry.get(&call.name);
                let mode_tool = self.mode_owner.is_some()
                    && tool
                        .as_ref()
                        .is_some_and(|tool| tool.is_trusted_mode_transition());
                if mode_tool {
                    transitions.push(index);
                    continue;
                }
                let safe = self.mode == SchedulerMode::Concurrent
                    && calls.len() > 1
                    && tool
                        .as_ref()
                        .map(|t| t.is_concurrency_safe(&call.input))
                        .transpose()?
                        .is_some_and(|v| truthy(&v))
                    && tool
                        .and_then(|t| t.is_read_only(&call.input).ok())
                        .is_some_and(|v| truthy(&v));
                if safe {
                    concurrent.push(index);
                } else {
                    sequential.push(index);
                }
            }
            let mut slots = vec![None; calls.len()];
            // Apply trusted mode transitions before admitting any sibling write.
            for index in transitions {
                let result = self
                    .execute(calls[index].clone(), self.fresh_mode(context.clone()))
                    .await?;
                if let Some(owner) = &self.mode_owner {
                    owner.apply_mode_effect(&result);
                }
                slots[index] = Some(result);
            }
            // Owned futures, not JoinHandles: early error, host abort, timeout and
            // Drop synchronously destroy every unfinished sibling future.
            let mut pending: Vec<_> = concurrent
                .into_iter()
                .map(|index| {
                    (
                        index,
                        Some(Box::pin(self.execute(
                            calls[index].clone(),
                            self.fresh_mode(context.clone()),
                        ))),
                    )
                })
                .collect();
            // TS Promise.all rejects on the first error and lets the started
            // siblings finish detached. Ledger: specs/gateway-parity/accepted-diffs.md
            // A6.5-TOOLS-CANCEL-001 (unapproved). Deliberate deviation: the rejection is
            // reported at the same moment, but the batch owns the siblings and
            // drops them. Only read-only concurrency-safe tools run here (see the
            // classification above), so no write is cut short, and a
            // non-cooperative sibling cannot hold the error back. The pass that
            // sees the error still polls every sibling once, as Promise.all's
            // synchronous map runs each up to its first await.
            poll_fn(|cx| {
                let mut unfinished = false;
                let mut first_error = None;
                for (index, slot) in &mut pending {
                    let Some(future) = slot else {
                        continue;
                    };
                    match future.as_mut().poll(cx) {
                        Poll::Ready(Ok(result)) => {
                            slots[*index] = Some(result);
                            *slot = None;
                        }
                        Poll::Ready(Err(error)) => {
                            *slot = None;
                            first_error.get_or_insert(error);
                        }
                        Poll::Pending => unfinished = true,
                    }
                }
                if let Some(error) = first_error {
                    Poll::Ready(Err(error))
                } else if unfinished {
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(()))
                }
            })
            .await?;
            for index in sequential {
                slots[index] = Some(
                    self.execute(calls[index].clone(), self.fresh_mode(context.clone()))
                        .await?,
                );
            }
            Ok(slots
                .into_iter()
                .map(|v| v.expect("every call has a result slot"))
                .collect())
        })
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
            let aliases = configured_aliases(context.fields.get("toolAliases"));
            repaired =
                registry::repair_tool_name(&call.name, &targets, aliases.as_ref())?.map(|r| r.name);
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
        // Ask policy is enforced before permission, including bypass mode.
        if context
            .fields
            .get("runMode")
            .and_then(JsValue::as_string)
            .is_some_and(|mode| mode.equals_str("ask"))
            && let Some(message) = super::ask_mode::violation(tool.as_ref(), &call.input)
        {
            return fail(&name, ToolError::new("ask_mode_violation", message));
        }
        #[cfg(unix)]
        let native_sandbox_enforced = self
            .native_sandbox
            .as_ref()
            .zip(self.native_sandbox_provider.as_ref())
            .is_some_and(|(mode, provider)| {
                mode.equals_str(provider.mode().as_str())
                    && ([
                        "bash",
                        "write_file",
                        "edit_file",
                        "edit_notebook",
                        "task_create",
                        "task_stop",
                    ]
                    .iter()
                    .any(|tool| name.equals_str(tool))
                        || tool.enforces_native_sandbox(&context, provider))
            });
        #[cfg(not(unix))]
        let native_sandbox_enforced = false;
        if let Some(mode) = self
            .native_sandbox
            .as_ref()
            .filter(|mode| mode.equals_str("read-only") || mode.equals_str("workspace-write"))
            && (name.equals_str("bash")
                || !matches!(tool.is_read_only(&call.input), Ok(JsValue::Bool(true))))
            && !native_sandbox_enforced
        {
            return fail(
                &name,
                ToolError::new(
                    "unsupported_tool",
                    format!(
                        "Native execution-world sandbox is unavailable for sandboxMode '{}'; refusing unrestricted {} execution.",
                        String::from_utf16_lossy(mode.units()),
                        String::from_utf16_lossy(name.units())
                    ),
                ),
            );
        }
        let operation = async {
            let issues = validate_input(
                &call.input,
                tool.fields()
                    .get("inputSchema")
                    .unwrap_or(&JsValue::Undefined),
            )?;
            if !issues.is_empty() {
                return fail(&name, validation_error(&name, issues, &context)?);
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
            let mut execute_input = call.input.clone();
            if let Some(result) = self
                .dispatch_tool_lifecycle("PreToolUse", &name, &call, &execute_input, &context, None)
                .await?
            {
                use crate::runtime::hooks::HookEffect;
                if let Some(error) = result
                    .blocking_errors
                    .iter()
                    .find(|error| error.code == "native_hook_output_limit")
                {
                    return fail(
                        &name,
                        ToolError::new("native_hook_output_limit", error.message.clone()),
                    );
                }
                for effect in &result.effects {
                    match effect {
                        HookEffect::Block { reason, .. } => {
                            return fail(
                                &name,
                                ToolError::new("permission_denied", reason.clone()),
                            );
                        }
                        HookEffect::PermissionDecision { behavior, reason }
                            if behavior.equals_str("deny") =>
                        {
                            return fail(
                                &name,
                                ToolError::new(
                                    "permission_denied",
                                    reason.clone().unwrap_or_else(|| {
                                        "PreToolUse hook denied the tool.".into()
                                    }),
                                ),
                            );
                        }
                        _ => {}
                    }
                }
                if let Some(input) = result.effects.iter().find_map(|effect| match effect {
                    HookEffect::UpdatedToolInput { input } => Some(input.clone()),
                    _ => None,
                }) {
                    execute_input = JsValue::Object(input);
                    let issues = validate_input(
                        &execute_input,
                        tool.fields()
                            .get("inputSchema")
                            .unwrap_or(&JsValue::Undefined),
                    )?;
                    if !issues.is_empty() {
                        return fail(&name, validation_error(&name, issues, &context)?);
                    }
                    // Recheck restrictions after a hook changes input.
                    if context
                        .fields
                        .get("runMode")
                        .and_then(JsValue::as_string)
                        .is_some_and(|v| v.equals_str("ask"))
                        && let Some(message) =
                            super::ask_mode::violation(tool.as_ref(), &execute_input)
                    {
                        return fail(&name, ToolError::new("ask_mode_violation", message));
                    }
                    if self.native_sandbox.as_ref().is_some_and(|mode| {
                        mode.equals_str("read-only") || mode.equals_str("workspace-write")
                    }) && (name.equals_str("bash")
                        || !matches!(tool.is_read_only(&execute_input), Ok(JsValue::Bool(true))))
                        && !native_sandbox_enforced
                    {
                        return fail(
                            &name,
                            ToolError::new(
                                "unsupported_tool",
                                "Native sandbox is unavailable for the updated tool input.",
                            ),
                        );
                    }
                }
            }
            match tool
                .validate_input(execute_input.clone(), context.clone())
                .await
            {
                Err(error) => return fail(&name, error),
                Ok(Some(issues)) => {
                    return fail(&name, validation_error(&name, issues, &context)?);
                }
                Ok(None) => {}
            }
            if let Some(plan) = context.host.plan_todo.as_ref().and_then(|value| {
                value.downcast_ref::<crate::host::modules::capability::PlanTodoContext>()
            }) {
                let read_only =
                    matches!(tool.is_read_only(&execute_input), Ok(JsValue::Bool(true)));
                if let Some(message) = plan.0.blocking_message(name.clone(), read_only).await? {
                    return fail(&name, ToolError::new("tool_execution_failed", message));
                }
            }
            if let Some(mode) = self
                .mode_owner
                .as_ref()
                .and_then(|owner| owner.current_mode())
            {
                self.permission.set_trusted_mode(mode);
            }
            let definition: Arc<dyn ToolDefinition> = tool.clone();
            let mut decision = if let Some(permission) = self.turn_permission.get() {
                permission
                    .decide_for_execution(definition, execute_input.clone(), call.id.clone())
                    .await?
            } else {
                self.permission
                    .decide(
                        definition,
                        execute_input.clone(),
                        Arc::new(context.clone()),
                        call.id.clone(),
                    )
                    .await?
            };
            if decision
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|v| v.equals_str("ask"))
                && let (Some(permission), Some(hooks)) =
                    (self.turn_permission.get(), self.turn_hooks.get())
            {
                use crate::host::modules::lifecycle::LifecyclePort;
                use crate::runtime::hooks::{HookEffect, LifecycleDispatchInput};
                let _grant_cleanup = GrantCleanup {
                    permission: permission.clone(),
                    tool: name.clone(),
                    call: call.id.clone(),
                    input: execute_input.clone(),
                };
                let cwd = context
                    .fields
                    .get("cwd")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                let base_input = object([
                    (
                        "sessionId",
                        context
                            .fields
                            .get("sessionId")
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    ),
                    ("transcriptPath", "".into()),
                    ("cwd", JsValue::String(cwd.clone())),
                    (
                        "permissionMode",
                        context
                            .fields
                            .get("permissionMode")
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    ),
                ]);
                let payload = object([
                    ("toolName", JsValue::String(name.clone())),
                    ("toolInput", execute_input.clone()),
                    ("toolUseId", JsValue::String(call.id.clone())),
                    (
                        "permissionSuggestions",
                        decision
                            .get("request")
                            .and_then(|r| r.get("options"))
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    ),
                ]);
                let dispatched = hooks
                    .dispatch(LifecycleDispatchInput {
                        event: "PermissionRequest".into(),
                        base_input,
                        payload: Some(payload),
                        match_query: Some(name.clone()),
                        cwd,
                        env: context
                            .fields
                            .get("env")
                            .and_then(JsValue::as_object)
                            .cloned(),
                        signal: None,
                    })
                    .await?;
                if let Some(result) = dispatched.effects.iter().find_map(|effect| match effect {
                    HookEffect::PermissionRequestResult { result } => Some(result),
                    _ => None,
                }) {
                    let behavior = result.get("behavior").and_then(JsValue::as_string);
                    if behavior.is_some_and(|v| v.equals_str("allow")) {
                        // The host callback minted the exact call/input grant.
                        // Re-evaluate native policy and consume it once; an
                        // unsupported unbacked hook allow stays ask/fail-closed.
                        let definition: Arc<dyn ToolDefinition> = tool.clone();
                        decision = permission
                            .decide_for_execution(
                                definition,
                                execute_input.clone(),
                                call.id.clone(),
                            )
                            .await?;
                    } else if behavior.is_some_and(|v| v.equals_str("deny")) {
                        decision = JsValue::Object(object([
                            ("type", "deny".into()),
                            (
                                "message",
                                result.get("message").cloned().unwrap_or(JsValue::Undefined),
                            ),
                        ]));
                    }
                }
            }
            let kind = decision.get("type").and_then(JsValue::as_string);
            let message = decision
                .get("message")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default();
            match kind.map(text).as_deref() {
                Some("deny") => {
                    self.dispatch_tool_lifecycle(
                        "PermissionDenied",
                        &name,
                        &call,
                        &execute_input,
                        &context,
                        Some(object([("reason", JsValue::String(message.clone()))])),
                    )
                    .await?;
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
                .unwrap_or_else(|| execute_input.clone());
            let mut execution = context.clone();
            let runtime = self.clone();
            let mut nested = context.clone();
            nested.execute_tool = None;
            nested.fields = nested
                .fields
                .entries()
                .iter()
                .filter(|(key, _)| {
                    !key.equals_str("currentToolCallId")
                        && !key.equals_str("currentPermissionDecision")
                })
                .fold(JsObject::new(), |mut fields, (key, value)| {
                    fields.insert(key.clone(), value.clone());
                    fields
                });
            execution.execute_tool = Some(Arc::new(move |call| {
                let runtime = runtime.clone();
                let context = nested.clone();
                Box::pin(async move {
                    runtime
                        .execute_batch(vec![call], context)
                        .await?
                        .into_iter()
                        .next()
                        .ok_or_else(|| ModuleError::new("Nested tool result was missing.").into())
                })
            }));
            execution
                .fields
                .insert("currentToolCallId".into(), string(call.id.clone()));
            execution
                .fields
                .insert("currentPermissionDecision".into(), decision);
            #[cfg(unix)]
            if let Some(provider) = &self.native_sandbox_provider {
                execution
                    .fields
                    .insert("nativeSandboxMode".into(), string(provider.mode().as_str()));
                execution.fields.insert(
                    "nativeSandboxTemporaryRoot".into(),
                    string(provider.temporary_root().to_string_lossy().as_ref()),
                );
            }
            // FileChanged observes the actual didSave notification from the write
            // body, not a path inferred from a tool name or response.
            if let Some(hooks) = self.turn_hooks.get().cloned() {
                use super::builtin::{FileUpdateNotice, FileUpdateNotifier};
                use crate::host::modules::lifecycle::LifecyclePort;
                let previous = execution.host.file_update_notifier.as_ref();
                if previous.is_some_and(|port| port.downcast_ref::<FileUpdateNotifier>().is_none())
                {
                    return fail(
                        &name,
                        ToolError::new(
                            "unsupported_tool",
                            "Unknown file update notifier cannot be safely composed.",
                        ),
                    );
                }
                let upstream = previous
                    .and_then(|port| port.downcast_ref::<FileUpdateNotifier>())
                    .map(|port| port.0.clone());
                let fields = context.fields.clone();
                let callback: Arc<dyn Fn(FileUpdateNotice) -> HostFuture<()> + Send + Sync> =
                    Arc::new(move |notice| {
                        let upstream = upstream.clone();
                        let hooks = hooks.clone();
                        let fields = fields.clone();
                        Box::pin(async move {
                            if let Some(upstream) = upstream {
                                let _ = upstream(notice.clone()).await;
                            }
                            let cwd = fields
                                .get("cwd")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_default();
                            let input = crate::runtime::hooks::LifecycleDispatchInput {
                                event: "FileChanged".into(),
                                base_input: object([
                                    (
                                        "sessionId",
                                        fields
                                            .get("sessionId")
                                            .cloned()
                                            .unwrap_or(JsValue::Undefined),
                                    ),
                                    ("transcriptPath", "".into()),
                                    ("cwd", JsValue::String(cwd.clone())),
                                    (
                                        "permissionMode",
                                        fields
                                            .get("permissionMode")
                                            .cloned()
                                            .unwrap_or(JsValue::Undefined),
                                    ),
                                ]),
                                payload: Some(object([
                                    ("absolutePath", JsValue::String(notice.absolute.clone())),
                                    ("relativePath", JsValue::String(notice.relative)),
                                    (
                                        "changeKind",
                                        if notice.created {
                                            "create".into()
                                        } else {
                                            "update".into()
                                        },
                                    ),
                                ])),
                                match_query: Some(notice.absolute),
                                cwd,
                                env: fields.get("env").and_then(JsValue::as_object).cloned(),
                                signal: None,
                            };
                            let _ = hooks.dispatch(input).await;
                            Ok(())
                        })
                    });
                let mut host = (*execution.host).clone();
                host.file_update_notifier = Some(Arc::new(FileUpdateNotifier(callback)));
                execution.host = Arc::new(host);
            }
            let track_progress =
                !name.equals_str("todo_write") && !truthy(&tool.is_read_only(&input)?);
            match tool.execute(input.clone(), execution).await {
                Err(error) => {
                    let extra = object([
                        ("error", JsValue::String(error.message.clone())),
                        ("isInterrupt", JsValue::Bool(error.code == "tool_aborted")),
                    ]);
                    if let Some(result) = self
                        .dispatch_tool_lifecycle(
                            "PostToolUseFailure",
                            &name,
                            &call,
                            &input,
                            &context,
                            Some(extra),
                        )
                        .await?
                        && result
                            .blocking_errors
                            .iter()
                            .any(|item| item.code == "native_hook_output_limit")
                    {
                        return fail(
                            &name,
                            ToolError::new(
                                "native_hook_output_limit",
                                "The tool failed and its PostToolUseFailure hook exceeded the native output limit.",
                            ),
                        );
                    }
                    fail(&name, error)
                }
                Ok(output) => {
                    let response = output
                        .data
                        .clone()
                        .unwrap_or_else(|| JsValue::Array(output.content.clone()));
                    let lifecycle = self
                        .dispatch_tool_lifecycle(
                            "PostToolUse",
                            &name,
                            &call,
                            &input,
                            &context,
                            Some(object([("toolResponse", response)])),
                        )
                        .await?;
                    let mut metadata = output.metadata;
                    if let Some(lifecycle) = lifecycle {
                        use crate::runtime::hooks::HookEffect;
                        if lifecycle
                            .blocking_errors
                            .iter()
                            .any(|error| error.code == "native_hook_output_limit")
                        {
                            return fail(
                                &name,
                                ToolError::new(
                                    "native_hook_output_limit",
                                    "The tool completed, but its PostToolUse hook exceeded the native output limit.",
                                ),
                            );
                        }
                        let mut info = JsObject::new();
                        let mut additional = Vec::new();
                        for effect in lifecycle.effects {
                            match effect {
                                HookEffect::Block {
                                    reason,
                                    stop_reason,
                                } if info.get("blocked").is_none() => {
                                    info.insert(
                                        "blocked".into(),
                                        JsValue::Object(object([
                                            ("reason", JsValue::String(reason)),
                                            (
                                                "stopReason",
                                                stop_reason
                                                    .map(JsValue::String)
                                                    .unwrap_or(JsValue::Undefined),
                                            ),
                                        ])),
                                    );
                                }
                                HookEffect::AdditionalContext { content, .. } => {
                                    additional.push(JsValue::String(content))
                                }
                                HookEffect::UpdatedMcpToolOutput { output }
                                    if info.get("updatedMcpToolOutput").is_none() =>
                                {
                                    info.insert("updatedMcpToolOutput".into(), output);
                                }
                                _ => {}
                            }
                        }
                        if !additional.is_empty()
                            || info.get("blocked").is_some()
                            || info.get("updatedMcpToolOutput").is_some()
                        {
                            info.insert("additionalContext".into(), JsValue::Array(additional));
                            metadata
                                .get_or_insert_with(JsObject::new)
                                .insert("lifecycle".into(), JsValue::Object(info));
                        }
                    }
                    let max_bytes = tool
                        .fields()
                        .get("maxResultBytes")
                        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                        .or_else(|| context.fields.get("maxResultBytes"))
                        .and_then(JsValue::as_number);
                    // A throw here is caught by TS's execute try block and
                    // becomes an ordinary execution-failure result.
                    let limit = match preview_metadata(&output.content, max_bytes) {
                        Ok(limit) => limit,
                        Err(error) => return fail(&name, ToolError::failed(error.message)),
                    };
                    if let Some(limit) = limit {
                        metadata
                            .get_or_insert_with(JsObject::new)
                            .insert("previewLimit".into(), JsValue::Object(limit));
                    }
                    if track_progress
                        && let Some(service) = context.host.plan_todo.as_ref()
                            .and_then(|value| value.downcast_ref::<crate::host::modules::capability::PlanTodoContext>())
                    {
                        let payload = object([
                            ("sessionId", context.fields.get("sessionId").cloned().unwrap_or(JsValue::Undefined)),
                            ("turnId", context.fields.get("turnId").cloned().unwrap_or(JsValue::Undefined)),
                            ("method", "mark_tool_progress".into()),
                            ("toolName", string(name.clone())),
                        ]);
                        if let Err(error) = service.0.call(JsValue::Object(object([("payload", JsValue::Object(payload))]))).await {
                            return fail(&name, ToolError::failed(error.message));
                        }
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
    fn applies_mode_effects(&self) -> bool {
        self.mode_owner.is_some()
    }
    fn bind_turn_permission(
        &self,
        permission: crate::runtime::project::interaction_permission::TurnPermission,
    ) -> Result<(), HostError> {
        permission.enable_native_execution()?;
        if let Some(owner) = &self.mode_owner {
            let owner = owner.clone();
            permission.bind_mode_source(Arc::new(move || owner.current_mode()))?;
        }
        self.turn_permission
            .set(permission)
            .map_err(|_| ModuleError::new("Tool execution is already bound to a turn.").into())
    }
    fn bind_turn_hooks(
        &self,
        hooks: crate::runtime::project::interaction_hooks::TurnHooks,
    ) -> Result<(), HostError> {
        self.turn_hooks.set(hooks).map_err(|_| {
            ModuleError::new("Tool execution hooks are already bound to a turn.").into()
        })
    }
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

/// repairToolName.ts: a falsy `toolAliases` is ignored; otherwise it is read
/// with `Object.entries`, which yields index keys for strings (UTF-16 code
/// units) and arrays, and nothing for other primitives.
fn configured_aliases(value: Option<&JsValue>) -> Option<JsObject> {
    let value = value.filter(|v| truthy(v))?;
    let mut aliases = JsObject::new();
    match value {
        JsValue::Object(o) => return Some(o.clone()),
        JsValue::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                aliases.insert(i.to_string().into(), item.clone());
            }
        }
        JsValue::String(s) => {
            for (i, unit) in s.units().iter().enumerate() {
                aliases.insert(
                    i.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        _ => {}
    }
    Some(aliases)
}

/// A plain object reached by validateObject. Property reads reuse the config
/// model (`config::changes`): own keys, the explicit prototype chain (an
/// explicit `null` prototype ends it without Object.prototype; an array
/// prototype adds `length`, indices and Array.prototype), then Object.prototype.
#[derive(Clone, Copy)]
enum Plain<'a> {
    Own(&'a JsObject),
    ObjectPrototype,
}
impl<'a> Plain<'a> {
    /// `key in object`.
    fn has(self, key: &JsString) -> bool {
        self.get(key).is_some()
    }
    /// `object[key]` when `key in object`; `Function` is an inherited method:
    /// it equals no enum member, matches no JSON type and is not a plain object.
    fn get(self, key: &JsString) -> Option<Operand<'a>> {
        match self {
            Plain::Own(o) => object_property(o, key),
            Plain::ObjectPrototype => object_prototype_property(key),
        }
    }
    /// `Object.keys`: integer keys ascending, then insertion order.
    fn keys(self) -> Vec<&'a JsString> {
        match self {
            Plain::Own(o) => js_entries(o).into_iter().map(|(k, _)| k).collect(),
            Plain::ObjectPrototype => vec![],
        }
    }
}
/// `schema.<key>` as a normal JS property read: own keys, then the explicit
/// prototype chain (schemas built programmatically or by config merges).
fn schema_field<'a>(schema: &'a JsValue, key: &str) -> Option<&'a JsValue> {
    match schema {
        JsValue::Object(o) => match object_property(o, &key.into())? {
            Operand::Value(v) => Some(v),
            _ => None,
        },
        _ => None,
    }
}
/// Own enumerable entries in JS `Object.keys`/`Object.entries` order.
fn js_entries(object: &JsObject) -> Vec<&(JsString, JsValue)> {
    let mut entries: Vec<_> = object.entries().iter().collect();
    entries.sort_by_key(|(key, _)| registry::array_index(key).map_or((1, 0), |n| (0, n)));
    entries
}
/// `String(value)` via the shared bounded conversion (`crate::js_value`):
/// nesting past `MAX_ARRAY_DEPTH` is V8's RangeError, raised as a host error
/// the way the TS call rejects, never a Rust stack overflow.
fn js_string(value: &JsValue) -> Result<JsString, HostError> {
    js_value::to_string_units(value)
        .map(JsString::from_utf16)
        .map_err(range_error)
}
fn js_display(value: &JsValue) -> Result<String, HostError> {
    js_value::display(value).map_err(range_error)
}
fn range_error(_: js_value::StackOverflow) -> HostError {
    ModuleError::new(js_value::StackOverflow::MESSAGE).into()
}
fn type_error(message: String) -> HostError {
    ModuleError::new(message).into()
}
fn not_iterable(value: &JsValue) -> HostError {
    let kind = match value {
        JsValue::Number(n) => format!("number {}", js_value::number_string(*n)),
        JsValue::Bool(b) => format!("boolean {b}"),
        _ => "object".into(),
    };
    type_error(format!(
        "{kind} is not iterable (cannot read property Symbol(Symbol.iterator))"
    ))
}

/// validateToolInput.ts, including the TypeErrors it throws for schema shapes
/// that are not JSON Schema (null schema, non-array enum, non-iterable
/// `required`, primitive `properties`).
fn validate_input(value: &JsValue, schema: &JsValue) -> Result<Vec<JsObject>, HostError> {
    fn issue(out: &mut Vec<JsObject>, path: &str, code: &str, message: String) {
        out.push(object([
            ("path", path.into()),
            ("code", code.into()),
            ("message", string(message)),
        ]));
    }
    /// matchesType: `type.some(...)` recursing into nested type arrays,
    /// depth-first like TS, iterative. Bounded one level above the shared
    /// String() limit: V8 survives this recursion deeper than String()
    /// (measured 3000 OK, 20000 RangeError), so the outer `type` array plus a
    /// member at the String() limit must still reach the description join.
    fn matches(value: Operand, kind: &JsValue) -> Result<bool, HostError> {
        let mut frames: Vec<std::slice::Iter<'_, JsValue>> = Vec::new();
        let mut next = Some(kind);
        loop {
            match next.take() {
                Some(JsValue::Array(kinds)) => {
                    if frames.len() > js_value::MAX_ARRAY_DEPTH {
                        return Err(range_error(js_value::StackOverflow));
                    }
                    frames.push(kinds.iter());
                }
                Some(kind) if matches_one(value, kind) => return Ok(true),
                _ => {}
            }
            let Some(kinds) = frames.last_mut() else {
                return Ok(false);
            };
            match kinds.next() {
                Some(kind) => next = Some(kind),
                None => {
                    frames.pop();
                }
            }
        }
    }
    fn matches_one(value: Operand, kind: &JsValue) -> bool {
        let number;
        let value = match value {
            Operand::Value(v) => v,
            Operand::Number(n) => {
                number = JsValue::Number(n);
                &number
            }
            Operand::Function(_) => return false,
            Operand::ObjectPrototype => {
                return kind.as_string().is_some_and(|s| s.equals_str("object"));
            }
        };
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
    /// `schema.properties ?? {}` as validateObject uses it.
    enum Properties<'a> {
        Object(&'a JsObject),
        Array(&'a [JsValue]),
        Primitive(&'a JsValue),
    }
    impl Properties<'_> {
        fn has(&self, key: &JsString) -> Result<bool, HostError> {
            match self {
                Properties::Object(o) => Ok(Plain::Own(o).has(key)),
                Properties::Array(a) => Ok(registry::array_index(key)
                    .is_some_and(|n| (n as usize) < a.len())
                    || key.equals_str("length")
                    || crate::config::changes::is_array_prototype_method(key)
                    || crate::config::builtin_property(key)),
                Properties::Primitive(v) => Err(type_error(format!(
                    "Cannot use 'in' operator to search for '{}' in {}",
                    text(key),
                    text(&js_string(v)?)
                ))),
            }
        }
        fn entries(&self) -> Vec<(JsString, JsValue)> {
            match self {
                Properties::Object(o) => js_entries(o).into_iter().cloned().collect(),
                Properties::Array(a) => a
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string().into(), v.clone()))
                    .collect(),
                Properties::Primitive(JsValue::String(s)) => s
                    .units()
                    .iter()
                    .enumerate()
                    .map(|(i, u)| {
                        (
                            i.to_string().into(),
                            JsValue::String(JsString::from_utf16(vec![*u])),
                        )
                    })
                    .collect(),
                Properties::Primitive(_) => vec![],
            }
        }
    }
    /// `for (const key of schema.required ?? [])`, keys coerced by `in`.
    fn required(schema: &JsValue) -> Result<Vec<JsString>, HostError> {
        match schema_field(schema, "required") {
            None | Some(JsValue::Null | JsValue::Undefined) => Ok(vec![]),
            Some(JsValue::Array(keys)) => keys.iter().map(js_string).collect(),
            Some(JsValue::String(s)) => {
                // String iteration yields code points; lone surrogates stand alone.
                let units = s.units();
                let mut keys = vec![];
                let mut i = 0;
                while i < units.len() {
                    let pair = (0xD800..0xDC00).contains(&units[i])
                        && units
                            .get(i + 1)
                            .is_some_and(|u| (0xDC00..0xE000).contains(u));
                    let width = if pair { 2 } else { 1 };
                    keys.push(JsString::from_utf16(units[i..i + width].to_vec()));
                    i += width;
                }
                Ok(keys)
            }
            Some(other) => Err(not_iterable(other)),
        }
    }
    fn walk(
        value: Operand,
        schema: &JsValue,
        path: &str,
        out: &mut Vec<JsObject>,
    ) -> Result<(), HostError> {
        match schema {
            JsValue::Null => {
                return Err(type_error(
                    "Cannot read properties of null (reading 'enum')".into(),
                ));
            }
            JsValue::Undefined => {
                return Err(type_error(
                    "Cannot read properties of undefined (reading 'enum')".into(),
                ));
            }
            _ => {}
        }
        match schema_field(schema, "enum") {
            Some(JsValue::Array(enumeration)) => {
                let equal = |v: &JsValue| match (value, v) {
                    (Operand::Value(JsValue::Number(a)), JsValue::Number(b)) => {
                        a.to_bits() == b.to_bits() || a.is_nan() && b.is_nan()
                    }
                    (Operand::Number(a), JsValue::Number(b)) => a.to_bits() == b.to_bits(),
                    (
                        Operand::Function(_)
                        | Operand::ObjectPrototype
                        | Operand::Number(_)
                        | Operand::Value(JsValue::Object(_) | JsValue::Array(_)),
                        _,
                    ) => false,
                    (Operand::Value(value), _) => v == value,
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
                                .collect::<Result<Vec<_>, _>>()?
                                .join(", ")
                        ),
                    );
                    return Ok(());
                }
            }
            Some(enumeration) if truthy(enumeration) => {
                return Err(type_error("schema.enum.some is not a function".into()));
            }
            _ => {}
        }
        let kind = schema_field(schema, "type");
        if let Some(kind) = kind
            && !matches(value, kind)?
        {
            let description = match kind {
                // Array#join prints null and undefined members as "".
                JsValue::Array(kinds) => kinds
                    .iter()
                    .map(|k| match k {
                        JsValue::Null | JsValue::Undefined => Ok(String::new()),
                        k => js_display(k),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join(" or "),
                kind => js_display(kind)?,
            };
            issue(
                out,
                path,
                "invalid_type",
                format!("{path} must be {description}."),
            );
            return Ok(());
        }
        let effective = match kind {
            Some(JsValue::Array(kinds)) => kinds
                .iter()
                .find(|k| k.as_string().is_none_or(|s| !s.equals_str("null"))),
            kind => kind,
        };
        let plain = match value {
            Operand::Value(JsValue::Object(o)) => Some(Plain::Own(o)),
            Operand::ObjectPrototype => Some(Plain::ObjectPrototype),
            _ => None,
        };
        if (effective.is_none()
            || effective
                .and_then(JsValue::as_string)
                .is_some_and(|s| s.equals_str("object")))
            && let Some(value) = plain
        {
            let empty = JsObject::new();
            let properties = match schema_field(schema, "properties") {
                None | Some(JsValue::Null | JsValue::Undefined) => Properties::Object(&empty),
                Some(JsValue::Object(o)) => Properties::Object(o),
                Some(JsValue::Array(a)) => Properties::Array(a),
                Some(other) => Properties::Primitive(other),
            };
            for key in required(schema)? {
                if !value.has(&key) {
                    let path = format!("{path}.{}", text(&key));
                    issue(out, &path, "required", format!("{path} is required."));
                }
            }
            if schema_field(schema, "additionalProperties") == Some(&JsValue::Bool(false)) {
                for key in value.keys() {
                    if !properties.has(key)? {
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
                if let Some(property) = value.get(&key) {
                    walk(property, &schema, &format!("{path}.{}", text(&key)), out)?;
                }
            }
        }
        if effective
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("array"))
            && let (Operand::Value(JsValue::Array(values)), Some(schema)) =
                (value, schema_field(schema, "items").filter(|s| truthy(s)))
        {
            for (i, value) in values.iter().enumerate() {
                walk(Operand::Value(value), schema, &format!("{path}[{i}]"), out)?;
            }
        }
        Ok(())
    }
    let mut issues = vec![];
    walk(Operand::Value(value), schema, "$", &mut issues)?;
    Ok(issues)
}
fn validation_error(
    name: &JsString,
    issues: Vec<JsObject>,
    context: &ToolRunContext,
) -> Result<ToolError, HostError> {
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
        return Ok(ToolError::new(
            "invalid_tool_input",
            format!("Tool {tool} input is invalid."),
        )
        .with_details(issues));
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
                .map(js_display)
                .transpose()?
                .map(|v| format!(" (current max_output_tokens: {v})"))
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
    Ok(ToolError::new("invalid_tool_input", message).with_details(issues))
}
fn content_text(v: &JsValue) -> Result<String, HostError> {
    let field = |key| {
        v.get(key)
            .map_or_else(|| Ok("undefined".into()), js_display)
    };
    let optional = |key| {
        v.get(key)
            .filter(|v| truthy(v))
            .map(|v| js_display(v).map(|v| format!(", {v}")))
            .transpose()
            .map(Option::unwrap_or_default)
    };
    Ok(
        match v
            .get("type")
            .and_then(JsValue::as_string)
            .map(text)
            .as_deref()
        {
            Some("text") => field("text")?,
            Some("json") => {
                pilotdeck_protocol::stringify_json(v.get("value").unwrap_or(&JsValue::Undefined))
                    .ok()
                    .flatten()
                    .unwrap_or_default()
            }
            Some("file") => format!(
                "[File: {}{}{}]",
                field("path")?,
                optional("mimeType")?,
                optional("description")?
            ),
            _ => String::new(),
        },
    )
}
/// applyResultSizeLimit's metadata. `Err` is the RangeError a too-deep
/// String() raises there; TS throws it inside the execute try block.
fn preview_metadata(content: &[JsValue], max: Option<f64>) -> Result<Option<JsObject>, HostError> {
    let Some(max) = max else {
        return Ok(None);
    };
    // TS `applyResultSizeLimit` tests `maxBytes < 0` (false for NaN) and then
    // reaches head/tail truncation with NaN budgets. The runtime only consumes
    // `.metadata`, so NaN reports a truncated body sized from the separator.
    if max < 0.0 {
        return Ok(None);
    }
    let binary = |v: &JsValue| {
        v.get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("image") || s.equals_str("pdf"))
    };
    let mut original = 0;
    for v in content {
        original += if binary(v) {
            v.get("data")
                .and_then(JsValue::as_string)
                .map_or(0, |s| s.units().len())
        } else {
            content_text(v)?.len()
        };
    }
    if content.iter().any(binary) {
        return Ok(Some(object([(
            "originalBytes",
            JsValue::Number(original as f64),
        )])));
    }
    if max.is_nan() {
        let suffix = "\n[Tool output truncated: head and tail shown.]";
        let separator = "\n... [middle omitted] ...\n";
        return Ok(Some(object([
            ("truncated", JsValue::Bool(true)),
            ("originalBytes", JsValue::Number(original as f64)),
            (
                "returnedBytes",
                JsValue::Number((separator.len() + original + suffix.len()) as f64),
            ),
        ])));
    }
    if original as f64 <= max {
        return Ok(None);
    }
    let suffix = "\n[Tool output truncated: head and tail shown.]";
    let budget = (max - suffix.len() as f64).max(0.0) as usize;
    let value = content
        .iter()
        .map(content_text)
        .collect::<Result<Vec<_>, _>>()?
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
    Ok(Some(object([
        ("truncated", JsValue::Bool(true)),
        ("originalBytes", JsValue::Number(original as f64)),
        ("returnedBytes", JsValue::Number(returned as f64)),
    ])))
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    fn text_content(value: &str) -> Vec<JsValue> {
        vec![JsValue::Object(object([
            ("type", JsValue::from("text")),
            ("text", JsValue::from(value)),
        ]))]
    }

    #[test]
    fn preview_metadata_negative_and_nan_limits_match_ts() {
        let content = text_content("abcdefghijklmnopqrstuvwxyz");
        assert!(
            preview_metadata(&content, Some(-1.0)).unwrap().is_none(),
            "maxBytes < 0 disables the limit, including the context fallback"
        );
        let metadata = preview_metadata(&content, Some(f64::NAN))
            .unwrap()
            .expect("NaN still runs head/tail truncation in TS");
        assert_eq!(
            pilotdeck_protocol::stringify_json(&JsValue::Object(metadata))
                .unwrap()
                .unwrap(),
            r#"{"truncated":true,"originalBytes":26,"returnedBytes":98}"#
        );
        let image = vec![JsValue::Object(object([
            ("type", JsValue::from("image")),
            ("data", JsValue::from("AAAA")),
        ]))];
        let metadata = preview_metadata(&image, Some(f64::NAN))
            .unwrap()
            .expect("binary metadata");
        assert_eq!(
            pilotdeck_protocol::stringify_json(&JsValue::Object(metadata))
                .unwrap()
                .unwrap(),
            r#"{"originalBytes":4}"#
        );
    }
}
