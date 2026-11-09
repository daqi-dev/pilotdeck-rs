//! Native one-shot forks use the same session, model, tool and transcript engines.
// ToolError is the shared tool boundary value and remains unboxed at this adapter.
#![allow(clippy::result_large_err)]
use super::{
    LiveSession, module_host,
    routes::live::LiveSessionState,
    sidecar_session::{SidecarSession, SidecarSessionConfig},
};
use crate::metadata::object;
use crate::{
    host::{
        HostError,
        modules::{
            capability::{RuntimeContext, ToolDefinition},
            event::EventSink,
            lifecycle,
            tool_context::{HostToolCheckpoint, OneShotSubagent, SeedState, SharedFileState},
        },
    },
    runtime::tools::{
        ToolError, ToolFuture, ToolRegistry,
        builtin::agent::{ForkReport, ForkRequest, SubagentFork, SubagentForkApi},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;
use std::time::Duration;

/// Project the public SDK agent definitions/settings into the child runtime contract.
#[cfg(test)]
pub(crate) fn apply_sdk_config(
    agent: &mut JsObject,
    sdk: Option<&JsValue>,
    baseline: Option<&JsValue>,
) {
    apply_sdk_config_with_policy(agent, sdk, baseline, None);
}

pub(crate) fn apply_sdk_config_with_policy(
    agent: &mut JsObject,
    sdk: Option<&JsValue>,
    baseline: Option<&JsValue>,
    organization: Option<&crate::runtime::sdk_session::OrganizationPolicy>,
) {
    if let Some(definitions) = sdk
        .and_then(|sdk| sdk.get("agents"))
        .and_then(JsValue::as_object)
    {
        let mut projected = JsObject::new();
        for (id, definition) in definitions.entries() {
            let mut fields = object_fields([
                ("id", JsValue::String(id.clone())),
                (
                    "description",
                    definition
                        .get("description")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                (
                    "systemPromptSuffix",
                    definition
                        .get("prompt")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                (
                    "allowedTools",
                    definition
                        .get("tools")
                        .cloned()
                        .unwrap_or_else(|| JsValue::Array(vec!["*".into()])),
                ),
                ("omitProjectInstructions", JsValue::Bool(false)),
                ("omitGitStatus", JsValue::Bool(false)),
                (
                    "isReadOnly",
                    JsValue::Bool(definition.get("permissionMode") == Some(&"plan".into())),
                ),
            ]);
            for key in [
                "disallowedTools",
                "effort",
                "permissionMode",
                "mcpServers",
                "skills",
                "memory",
                "initialPrompt",
                "background",
                "observer",
            ] {
                if let Some(value) = definition.get(key).filter(|v| !v.is_undefined()) {
                    fields.insert(key.into(), value.clone());
                }
            }
            let requested_max_turns = definition.get("maxTurns").and_then(JsValue::as_number);
            if let Some(max_turns) = cap_number(
                requested_max_turns,
                organization.and_then(|policy| policy.max_turns),
            ) {
                fields.insert("maxTurns".into(), JsValue::Number(max_turns));
            }
            if let Some(model) = definition.get("model").and_then(sdk_model) {
                fields.insert("modelOverride".into(), JsValue::Object(model));
            }
            if let Some(message) = definition
                .get("observerMessage")
                .and_then(JsValue::as_string)
            {
                let message = text(message)
                    .trim_matches(crate::metadata::js_space)
                    .to_owned();
                if !message.is_empty() {
                    fields.insert("observerMessage".into(), JsValue::String(message.into()));
                }
            }
            if let Some(reminder) = definition.get("criticalSystemReminder_EXPERIMENTAL") {
                fields.insert("criticalSystemReminder".into(), reminder.clone());
            }
            projected.insert(id.clone(), JsValue::Object(fields));
        }
        agent.insert("subagentDefinitions".into(), JsValue::Object(projected));
    }
    let configured = sdk
        .and_then(|sdk| sdk.get("settings"))
        .and_then(|settings| settings.get("agent"))
        .and_then(|agent| agent.get("subagents"));
    let base = baseline.and_then(|agent| agent.get("subagents"));
    let timeout = configured
        .and_then(|value| value.get("timeoutMs"))
        .or_else(|| base.and_then(|value| value.get("timeoutMs")))
        .and_then(JsValue::as_number);
    if let Some(timeout) = cap_number(
        timeout,
        organization.and_then(|policy| policy.max_subagent_timeout_ms),
    ) {
        agent.insert("subagentTimeoutMs".into(), JsValue::Number(timeout));
    }
    let depth = configured
        .and_then(|value| value.get("maxDepth"))
        .or_else(|| base.and_then(|value| value.get("maxDepth")))
        .and_then(JsValue::as_number);
    if let Some(cap) = organization.and_then(|policy| policy.max_subagent_depth) {
        agent.insert(
            "maxSubagentDepth".into(),
            JsValue::Number(depth.unwrap_or(1.0).min(cap)),
        );
    } else if let Some(depth) = depth {
        agent.insert("maxSubagentDepth".into(), JsValue::Number(depth));
    }
    let model = configured
        .and_then(|v| v.get("default"))
        .or_else(|| base.and_then(|v| v.get("default")));
    if let Some(model) = model {
        if matches!(model, JsValue::Null) {
            agent.insert("subagentModel".into(), JsValue::Undefined);
        } else {
            let model = sdk_model(model)
                .or_else(|| model.as_object().cloned())
                .unwrap_or_else(|| {
                    object_fields([
                        (
                            "provider",
                            agent.get("provider").cloned().unwrap_or(JsValue::Undefined),
                        ),
                        (
                            "model",
                            agent.get("model").cloned().unwrap_or(JsValue::Undefined),
                        ),
                    ])
                });
            agent.insert("subagentModel".into(), JsValue::Object(model));
        }
    }
}

fn cap_number(value: Option<f64>, cap: Option<f64>) -> Option<f64> {
    match (value, cap) {
        (Some(value), Some(cap)) => Some(value.min(cap)),
        (Some(value), None) => Some(value),
        (None, Some(cap)) => Some(cap),
        (None, None) => None,
    }
}

fn sdk_model(value: &JsValue) -> Option<JsObject> {
    let value = value.as_string()?.to_utf8().ok()?;
    let (provider, model) = value.split_once('/')?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some(object_fields([
        ("provider", provider.into()),
        ("model", model.into()),
    ]))
}

#[derive(Clone)]
pub struct NativeSubagentPort {
    snapshot: JsValue,
    input: JsValue,
    registry: ToolRegistry,
    events: Arc<module_host::EventBuffer>,
    live: Option<Arc<LiveSessionState>>,
    services: Option<Arc<crate::runtime::turn::services::NativeSessionServices>>,
    interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
    stats: Option<crate::runtime::turn::routes::usage::SharedUsageStats>,
    mcp: Option<Arc<crate::runtime::manager::session_mcp::SessionMcpOwner>>,
    lifecycle: crate::runtime::hooks::LifecycleRuntime,
    context_resources: Option<Arc<super::context_resources::ContextResources>>,
    subagent_providers: Option<
        Arc<crate::runtime::session::subagent_continuation::NamedContinuationProviderRegistry>,
    >,
}

impl NativeSubagentPort {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        snapshot: JsValue,
        input: JsValue,
        registry: ToolRegistry,
        events: Arc<module_host::EventBuffer>,
        live: Option<Arc<LiveSessionState>>,
        services: Option<Arc<crate::runtime::turn::services::NativeSessionServices>>,
        interactions: Option<Arc<crate::runtime::interaction::GatewayInteractionCoordinator>>,
        stats: Option<crate::runtime::turn::routes::usage::SharedUsageStats>,
        lifecycle: crate::runtime::hooks::LifecycleRuntime,
    ) -> Self {
        Self {
            snapshot,
            input,
            registry,
            events,
            live,
            services,
            interactions,
            stats,
            mcp: module_host::current_mcp_owner(),
            lifecycle,
            context_resources: module_host::current_context_resources(),
            subagent_providers: module_host::current_subagent_providers(),
        }
    }
    fn config(&self) -> JsObject {
        self.input
            .get("agent")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap_or_default()
    }
    fn definitions(&self) -> Vec<Definition> {
        if let Some(definitions) = self
            .input
            .get("agent")
            .and_then(|v| v.get("subagentDefinitions"))
            .and_then(JsValue::as_object)
        {
            return definitions
                .entries()
                .iter()
                .map(|(id, value)| Definition::parse(id, value))
                .collect();
        }
        ["general-purpose", "explore", "plan", "verify"]
            .into_iter()
            .map(Definition::builtin)
            .collect()
    }
    fn definition(&self, key: &JsString) -> Option<Definition> {
        self.definitions()
            .into_iter()
            .find(|definition| &definition.key == key)
    }
    fn emit(
        &self,
        kind: &str,
        request: &ForkRequest,
        entries: &[(&str, JsValue)],
    ) -> Result<(), ToolError> {
        let mut value = JsObject::new();
        value.insert("type".into(), kind.into());
        for key in ["sessionId", "turnId"] {
            value.insert(
                key.into(),
                self.input.get(key).cloned().unwrap_or(JsValue::Undefined),
            );
        }
        value.insert(
            "subagentId".into(),
            JsValue::String(request.subagent_id.clone()),
        );
        value.insert(
            "subagentType".into(),
            JsValue::String(request.definition_id.clone()),
        );
        for (key, item) in entries {
            value.insert((*key).into(), item.clone());
        }
        self.events.emit(value).map_err(failed)
    }
    fn record(
        &self,
        kind: &str,
        request: &ForkRequest,
        entries: &[(&str, JsValue)],
    ) -> Result<(), ToolError> {
        let writer = self
            .live
            .as_ref()
            .map(|v| v.recorder.writer())
            .or_else(|| self.services.as_ref().map(|s| &s.writer));
        if let Some(writer) = writer {
            let mut draft = JsObject::new();
            draft.insert("type".into(), kind.into());
            draft.insert(
                "subagentId".into(),
                JsValue::String(request.subagent_id.clone()),
            );
            draft.insert(
                "subagentType".into(),
                JsValue::String(request.definition_id.clone()),
            );
            for (key, value) in entries {
                if *key == "prompt" || *key == "summary" {
                    let limit = if *key == "prompt" { 1024 } else { 4096 };
                    let (preview, truncated) =
                        preview(value.as_string().unwrap_or(&JsString::default()), limit);
                    draft.insert(format!("{key}Preview").into(), JsValue::String(preview));
                    draft.insert(format!("{key}Truncated").into(), JsValue::Bool(truncated));
                } else {
                    draft.insert((*key).into(), value.clone());
                }
            }
            writer
                .record_session_event(
                    &value_string(&self.input, "sessionId"),
                    &value_string(&self.input, "turnId"),
                    draft,
                )
                .map_err(failed)?;
        }
        Ok(())
    }
    async fn lifecycle(
        &self,
        name: &str,
        request: &ForkRequest,
        success: Option<bool>,
    ) -> Result<(), ToolError> {
        let config = self.config();
        let mut payload = JsObject::new();
        payload.insert(
            "subagentId".into(),
            JsValue::String(request.subagent_id.clone()),
        );
        payload.insert(
            "subagentType".into(),
            JsValue::String(request.definition_id.clone()),
        );
        if let Some(success) = success {
            payload.insert("success".into(), JsValue::Bool(success));
        }
        let handler = lifecycle::Handler::new(
            self.lifecycle.clone(),
            lifecycle::Binding {
                session_id: value_string(&self.input, "sessionId"),
                turn_id: value_string(&self.input, "turnId"),
                cwd: string_field(&config, "cwd"),
                permission_mode: config
                    .get("permissionMode")
                    .and_then(JsValue::as_string)
                    .cloned(),
                base_env: config.get("env").and_then(JsValue::as_object).cloned(),
                signal: request.context.cancellation().cloned(),
            },
        );
        handler
            .call(object([(
                "payload",
                object([
                    ("operation", "dispatch".into()),
                    ("event", name.into()),
                    ("payload", JsValue::Object(payload)),
                ]),
            )]))
            .await
            .map_err(failed)?;
        Ok(())
    }

    fn run(&self, request: ForkRequest, definition: Definition) -> ToolFuture<ForkReport> {
        let port = self.clone();
        Box::pin(async move { port.run_inner(request, definition).await })
    }

    async fn run_inner(
        &self,
        mut request: ForkRequest,
        definition: Definition,
    ) -> Result<ForkReport, ToolError> {
        let _provider_run = if let Some(registry) = &self.subagent_providers {
            Some(
                registry
                    .lease("pilotdeck-native")
                    .map_err(|error| ToolError::failed(error.message))?,
            )
        } else {
            None
        };
        let started = std::time::Instant::now();
        let compose_abort = crate::runtime::network::Cancellation::new();
        if let Some(parent) = request.context.cancellation().cloned() {
            let composed = compose_abort.clone();
            tokio::spawn(async move {
                parent.cancelled().await;
                composed.cancel();
            });
        }
        request.definition_id = definition.id.clone();
        let observer = match &definition.observer {
            Some(id) => {
                let found = self.definition(id).ok_or_else(|| {
                    ToolError::failed(format!(
                        "Observer definition {} for {} is not configured.",
                        text(id),
                        text(&definition.id)
                    ))
                })?;
                if self.services.is_none() {
                    return Err(ToolError::failed(format!(
                        "Observer definition {} requires a host observer-subagent launcher.",
                        text(&found.id)
                    )));
                }
                Some(found)
            }
            None => None,
        };
        let config = self.config();
        let cwd = string_field(&config, "cwd");
        let session_id: JsString =
            format!("{}::sub::{}", text(&cwd), text(&request.subagent_id)).into();
        let turn_id: JsString = format!("{}-t0", text(&request.subagent_id)).into();
        let parent_writer = self
            .live
            .as_ref()
            .map(|v| v.recorder.writer().clone())
            .or_else(|| self.services.as_ref().map(|s| s.writer.clone()));
        let sidechain = parent_writer
            .as_ref()
            .map(|writer| writer.for_subagent(&request.subagent_id));
        let relative = parent_writer
            .as_ref()
            .map(|writer| JsString::from(writer.relative_subagent_path(&request.subagent_id)));
        let seed = SeedState {
            allowed_read_files: Some(request.context.host.files.allowed_read_files.clone()),
            read_file_state: Some(
                request
                    .context
                    .host
                    .files
                    .read_file_state
                    .lock()
                    .map_err(failed)?
                    .clone(),
            ),
            write_snapshots: Some(
                request
                    .context
                    .host
                    .files
                    .write_snapshots
                    .lock()
                    .map_err(failed)?
                    .clone(),
            ),
        };
        let checkpoint = HostToolCheckpoint::new(Some(&seed), &[]).map_err(failed)?;
        let child_live = match (sidechain, &self.live) {
            (Some(sidechain), Some(parent)) => Some(LiveSessionState::for_subagent(
                session_id.clone(),
                cwd.clone(),
                Arc::new(sidechain.writer),
                checkpoint,
                parent.file_history.clone(),
            )),
            (Some(sidechain), None) => {
                let writer = Arc::new(sidechain.writer);
                let history = crate::storage::file_history::create_recorded_file_history(
                    crate::storage::file_history::FileHistoryOptions::new(
                        sidechain.transcript_path.with_extension("file-history"),
                    ),
                    Arc::new(crate::storage::recorder::Recorder::from_writer(
                        writer.clone(),
                    )),
                    session_id.clone(),
                    &[],
                );
                Some(LiveSessionState::for_subagent(
                    session_id.clone(),
                    cwd.clone(),
                    writer,
                    checkpoint,
                    history,
                ))
            }
            _ => None,
        };
        let child_config = definition.config(&config, &request.subagent_id);
        let child_depth = child_config
            .get("subagentDepth")
            .and_then(JsValue::as_number)
            .unwrap_or(2.0);
        let max_depth = config
            .get("maxSubagentDepth")
            .and_then(JsValue::as_number)
            .unwrap_or(1.0);
        let allowed = definition.allowed.clone();
        let denied = definition.denied.clone();
        let registry = self
            .registry
            .scoped(Arc::new(move |tool| {
                let name = tool
                    .fields()
                    .get("name")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                (allowed.iter().any(|v| v.equals_str("*")) || allowed.contains(&name))
                    && !denied.contains(&name)
                    && (!tool
                        .required_runtime_capabilities()
                        .is_some_and(|v| v.contains(&"subagent_fork".into()))
                        || child_depth < max_depth)
            }))
            .map_err(failed)?;
        let child_resources = self.context_resources.as_ref().map(|resources| {
            Arc::new(
                resources.for_subagent(definition.skills.as_deref(), definition.memory_disabled),
            )
        });
        install_definition_skill_scope(&definition, &registry, child_resources.as_deref())
            .map_err(failed)?;
        if let Some(parent_agent) = self.registry.get(&"agent".into())
            && let Some(described) = crate::runtime::tools::builtin::agent::for_child_scope(
                parent_agent,
                max_depth,
                child_depth,
            )
        {
            registry.register_or_replace(described).map_err(failed)?;
        }
        let mut definition_mcp = DefinitionMcp::open(&definition, &registry).await?;
        let mut messages = Vec::new();
        if let Some(initial) = definition
            .initial
            .as_ref()
            .map(text)
            .map(|v| v.trim_matches(crate::metadata::js_space).to_owned())
            .filter(|v| !v.is_empty())
        {
            messages.push(message(initial.into()));
        }
        messages.push(message(request.directive.clone()));
        let mut input = JsObject::new();
        input.insert("agent".into(), JsValue::Object(child_config));
        input.insert(
            "permissionContext".into(),
            input
                .get("agent")
                .and_then(|a| a.get("permissionContext"))
                .cloned()
                .unwrap_or_else(|| object([])),
        );
        input.insert(
            "interactionCapabilities".into(),
            object([("elicitationAvailable", JsValue::Bool(false))]),
        );
        input.insert("messages".into(), JsValue::Array(messages.clone()));
        input.insert(
            "seedState".into(),
            JsValue::Object(crate::host::modules::tool_context::serialize_seed_state(
                &seed,
            )),
        );
        input.insert("canElicit".into(), JsValue::Bool(false));
        if let Some(can_prompt) = self.input.get("canPrompt") {
            input.insert("canPrompt".into(), can_prompt.clone());
        }
        input.insert(
            "permissionSessionId".into(),
            self.input
                .get("permissionSessionId")
                .cloned()
                .unwrap_or_else(|| JsValue::String(value_string(&self.input, "sessionId"))),
        );
        if let Some(max) = definition.max_turns {
            input.insert("maxTurns".into(), JsValue::Number(max));
        }
        let port = self.clone();
        let parent_session = value_string(&self.input, "sessionId");
        let parent_turn = value_string(&self.input, "turnId");
        let child_id = request.subagent_id.clone();
        let child_type = request.definition_id.clone();
        let last_assistant = Arc::new(std::sync::Mutex::new(JsString::default()));
        let activities = Arc::new(std::sync::Mutex::new(Vec::<JsValue>::new()));
        let activity_digest = activities.clone();
        let assistant = last_assistant.clone();
        let activity = Arc::new(move |event: JsValue| -> Result<(), HostError> {
            let mut digest = activity_digest.lock().unwrap();
            if digest.len() < 64 {
                digest.push(event.clone());
            }
            drop(digest);
            let kind = event
                .get("type")
                .and_then(JsValue::as_string)
                .map(text)
                .unwrap_or_default();
            if kind == "assistant_message" {
                let value = assistant_text(event.get("message").unwrap_or(&JsValue::Undefined));
                if has_text(event.get("message").unwrap_or(&JsValue::Undefined)) {
                    *assistant.lock().unwrap() = value;
                }
                if let Some(JsValue::Array(blocks)) =
                    event.get("message").and_then(|m| m.get("content"))
                {
                    for block in blocks {
                        if (block.get("type") == Some(&"text".into())
                            || block.get("type") == Some(&"thinking".into()))
                            && block.get("timeline").is_some_and(truthy)
                        {
                            port.events.emit(object_fields([
                                ("type", "agent_status".into()),
                                ("sessionId", JsValue::String(parent_session.clone())),
                                ("turnId", JsValue::String(parent_turn.clone())),
                                ("subagentId", JsValue::String(child_id.clone())),
                                ("subagentType", JsValue::String(child_type.clone())),
                                ("event", "subagent_assistant_block".into()),
                                (
                                    "timeline",
                                    block.get("timeline").cloned().unwrap_or(JsValue::Undefined),
                                ),
                                (
                                    "detail",
                                    object([
                                        ("subagentId", JsValue::String(child_id.clone())),
                                        (
                                            "kind",
                                            block
                                                .get("type")
                                                .cloned()
                                                .unwrap_or(JsValue::Undefined),
                                        ),
                                        (
                                            "text",
                                            block
                                                .get("text")
                                                .cloned()
                                                .unwrap_or(JsValue::Undefined),
                                        ),
                                        (
                                            "blockId",
                                            block
                                                .get("blockId")
                                                .cloned()
                                                .unwrap_or(JsValue::Undefined),
                                        ),
                                    ]),
                                ),
                            ]))?;
                        }
                    }
                }
            }
            if kind == "compact_started" || kind == "compact_completed" {
                let mut detail = event.as_object().cloned().unwrap_or_default();
                detail.insert("subagentId".into(), JsValue::String(child_id.clone()));
                port.events.emit(object_fields([
                    ("type", "agent_status".into()),
                    ("sessionId", JsValue::String(parent_session.clone())),
                    ("turnId", JsValue::String(parent_turn.clone())),
                    ("subagentId", JsValue::String(child_id.clone())),
                    ("subagentType", JsValue::String(child_type.clone())),
                    ("event", JsValue::String(format!("subagent_{kind}").into())),
                    (
                        "timeline",
                        event.get("timeline").cloned().unwrap_or(JsValue::Undefined),
                    ),
                    ("detail", JsValue::Object(detail)),
                ]))?;
            }
            let projected = match kind.as_str() {
                "model_event" => Some("subagent_model_event"),
                "tool_calls_detected" => Some("subagent_tool_calls_detected"),
                "tool_result" => Some("subagent_tool_result"),
                "permission_request" => Some("permission_request"),
                _ => None,
            };
            if let Some(projected) = projected {
                let mut fields = event.as_object().cloned().unwrap_or_default();
                fields.insert("type".into(), projected.into());
                fields.insert("sessionId".into(), JsValue::String(parent_session.clone()));
                fields.insert("turnId".into(), JsValue::String(parent_turn.clone()));
                fields.insert("subagentId".into(), JsValue::String(child_id.clone()));
                fields.insert("subagentType".into(), JsValue::String(child_type.clone()));
                port.events.emit(fields)?;
            }
            Ok(())
        });
        let mut session = SidecarSession::new(
            SidecarSessionConfig {
                limits: crate::host::Limits {
                    execution_timeout: Some(
                        crate::runtime::network::node_timer_duration(request.timeout_ms)
                            .saturating_add(std::time::Duration::from_secs(10)),
                    ),
                    ..Default::default()
                },
                session_key: session_id.clone(),
                project_key: None,
                cwd: cwd.clone(),
                snapshot: self.snapshot.clone(),
                services: self.services.clone(),
            },
            Arc::new(NativeSpawner),
        )
        .with_fork_scope(
            registry,
            child_live.clone(),
            activity,
            self.lifecycle.clone(),
        )
        .with_fork_router_abort(compose_abort.clone())
        .with_mcp_owner(self.mcp.clone());
        if let Some(providers) = &self.subagent_providers {
            session = session.with_subagent_providers(providers.clone());
        }
        if let Some(resources) = child_resources {
            session = session.with_context_resources(resources);
        }
        if let Some(interactions) = &self.interactions {
            session = session.with_interactions(interactions.clone());
        }
        if let Some(stats) = &self.stats {
            session = session.with_usage_stats(stats.clone());
        }
        // Child elicitation is intentionally absent, but policy/session allow rules are shared.
        let session = Arc::new(session);
        let mut guard = ChildGuard {
            session: Some(session.clone()),
            executor: tokio::runtime::Handle::current(),
            live: child_live.clone(),
            port: self.clone(),
            subagent_id: request.subagent_id.clone(),
            subagent_type: request.definition_id.clone(),
            started_recorded: false,
            lifecycle_started: false,
            event_started: false,
            completed_recorded: false,
            lifecycle_stopped: false,
            event_completed: false,
        };
        self.record(
            "subagent_started",
            &request,
            &[
                ("prompt", JsValue::String(request.directive.clone())),
                (
                    "transcriptRelativePath",
                    relative.clone().map_or("".into(), JsValue::String),
                ),
                ("subagentSessionId", JsValue::String(session_id.clone())),
            ],
        )?;
        guard.started_recorded = true;
        let result = async {
            self.lifecycle("SubagentStart", &request, None).await?;
            guard.lifecycle_started = true;
            self.emit("subagent_started", &request, &[("toolCallId", request.tool_call_id.clone().map_or(JsValue::Undefined, JsValue::String))])?;
            guard.event_started = true;
            if let Some(live) = &child_live {
                live.record_event(&object([("type", "turn_started".into()), ("sessionId", JsValue::String(session_id.clone())), ("turnId", JsValue::String(turn_id.clone()))])).map_err(failed)?;
                live.recorder.writer().record_session_event(&session_id, &turn_id, object_fields([("type", "subagent_descriptor".into()), ("descriptor", object([("version", JsValue::Number(1.0)), ("mode", "one-shot".into()), ("provider", "pilotdeck-native".into()), ("definitionId", JsValue::String(request.definition_id.clone()))]))])).map_err(failed)?;
                live.record_event(&object([("type", "input_accepted".into()), ("sessionId", JsValue::String(session_id.clone())), ("turnId", JsValue::String(turn_id.clone())), ("messages", JsValue::Array(messages))])).map_err(failed)?;
            }
            let deadline = tokio::time::Instant::now()
                + crate::runtime::network::node_timer_duration(request.timeout_ms);
            let mut turn = session.submit(JsValue::Object(input), object([("turnId", JsValue::String(turn_id.clone()))])).map_err(failed)?;
            let timer = tokio::time::sleep_until(deadline);
            tokio::pin!(timer);
            let mut terminal = None;
            let mut timed_out = false;
            loop {
                let next = tokio::select! {
                    biased;
                    _ = &mut timer => {
                        timed_out = true;
                        compose_abort.cancel_synchronously();
                        let _ = session.abort(&"subagent_timeout".into());
                        break;
                    }
                    _ = request.context.cancelled() => { return Err(ToolError::failed("Subagent aborted before completion.")); }
                    next = turn.next() => next.map_err(failed)?,
                };
                let Some(event) = next else { break; };
                if event.get("type") == Some(&JsValue::from("turn_completed")) { terminal = event.get("result").cloned(); }
            }
            if timed_out {
                let drain_until = tokio::time::Instant::now() + Duration::from_secs(2);
                while tokio::time::Instant::now() < drain_until {
                    match tokio::time::timeout(Duration::from_millis(50), turn.next()).await {
                        Ok(Ok(None)) => break,
                        Ok(Ok(Some(_))) => continue,
                        _ => break,
                    }
                }
                let _ = session.dispose().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Err(ToolError::failed(format!(
                    "Subagent timed out after {}ms.",
                    crate::js_value::number_string(request.timeout_ms)
                )));
            }
            let terminal = terminal.ok_or_else(|| ToolError::failed("SubAgentSession: AgentLoop returned no result"))?;
            if terminal.get("type") == Some(&JsValue::from("aborted")) { return Err(ToolError::failed(format!("SubAgentSession: subagent turn aborted ({})", text(&value_string(&terminal, "stopReason"))))); }
            if terminal.get("type") == Some(&JsValue::from("error")) {
                let details = match terminal.get("errors") { Some(JsValue::Array(errors)) => errors.iter().map(|e| text(&value_string(e, "message"))).collect::<Vec<_>>().join("; "), _ => terminal.get("error").map(|e| pilotdeck_protocol::stringify_json(e).ok().flatten().unwrap_or_default()).unwrap_or_default() };
                return Err(ToolError::failed(format!("SubAgentSession: subagent turn failed ({}){}", text(&value_string(&terminal, "stopReason")), if details.is_empty() { String::new() } else { format!(": {details}") })));
            }
            let markdown = last_assistant.lock().unwrap().clone();
            let parsed = parse_summary(&markdown);
            Ok(ForkReport { markdown, usage: terminal.get("usage").cloned().unwrap_or_else(|| object([])), turns: terminal.get("turns").and_then(JsValue::as_number).unwrap_or(0.0), duration_ms: started.elapsed().as_secs_f64() * 1000.0, parsed, subagent_session_id: Some(session_id), transcript_relative_path: relative })
        }.await;
        if result.is_err() {
            let _ = session.abort(&"subagent_settled".into());
        }
        let disposed = session.dispose().await.map_err(failed);
        let mcp_disposed = definition_mcp.dispose().await;
        let live_disposed = child_live
            .as_ref()
            .map(|live| live.dispose().map_err(failed))
            .unwrap_or(Ok(()));
        let mut result = match (result, disposed.and(mcp_disposed).and(live_disposed)) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) | (_, Err(error)) => Err(error),
        };
        if started.elapsed() >= crate::runtime::network::node_timer_duration(request.timeout_ms) {
            result = Err(ToolError::failed(format!(
                "Subagent timed out after {}ms.",
                crate::js_value::number_string(request.timeout_ms)
            )));
        }
        let recorded = match &result {
            Ok(report) => self.record(
                "subagent_completed",
                &request,
                &[
                    ("summary", JsValue::String(report.markdown.clone())),
                    ("usage", report.usage.clone()),
                    ("turns", JsValue::Number(report.turns)),
                    ("durationMs", JsValue::Number(report.duration_ms)),
                    ("errored", JsValue::Bool(false)),
                ],
            ),
            Err(error) => self.record(
                "subagent_completed",
                &request,
                &[
                    ("summary", JsValue::String(error.message.clone())),
                    ("turns", JsValue::Number(0.0)),
                    ("durationMs", JsValue::Number(0.0)),
                    ("errored", JsValue::Bool(true)),
                ],
            ),
        };
        guard.completed_recorded = true;
        if let Err(error) = recorded {
            result = Err(error);
        }
        if guard.lifecycle_started {
            let stopped = self
                .lifecycle("SubagentStop", &request, Some(result.is_ok()))
                .await;
            guard.lifecycle_stopped = true;
            if let Err(error) = stopped {
                result = Err(error);
            }
        }
        if guard.event_started {
            let emitted = self.emit(
                "subagent_completed",
                &request,
                &[
                    ("success", JsValue::Bool(result.is_ok())),
                    ("aborted", JsValue::Bool(request.context.is_aborted())),
                    (
                        "durationMs",
                        JsValue::Number(result.as_ref().map_or(0.0, |r| r.duration_ms)),
                    ),
                ],
            );
            guard.event_completed = true;
            if let Err(error) = emitted {
                result = Err(error);
            }
        }
        guard.session.take();
        if let Some(observer) = observer {
            let observer = prepare_observer(observer);
            let runtime = self.services.as_ref().unwrap().background.subagents.clone();
            let observer_id: JsString = uuid::Uuid::new_v4().to_string().into();
            let observed_id = request.subagent_id.clone();
            let observed_type = request.definition_id.clone();
            let outcome = match &result {
                Ok(report) => format!(
                    "Outcome: completed\nObserved final report:\n{}",
                    truncate_observer(&text(&report.markdown), 4000)
                ),
                Err(error) => format!(
                    "Outcome: failed\nObserved error: {}",
                    truncate_observer(&text(&error.message), 1000)
                ),
            };
            let activity = activities
                .lock()
                .unwrap()
                .iter()
                .filter_map(activity_line)
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            let digest = format!(
                "You are an observer. Do not execute the observed task or attempt to change its outcome.\nReview this read-only activity digest and produce a concise report of risks, failures, or notable findings.\n\nObserved agent: {}\nObserved directive: {}\n\nActivity:\n{}\n\n{}{}",
                text(&definition.id),
                truncate_observer(&text(&request.directive), 2000),
                if activity.is_empty() {
                    "- No observable model or tool activity was recorded.".into()
                } else {
                    activity
                },
                outcome,
                definition
                    .observer_message
                    .as_ref()
                    .map(text)
                    .map(|message| message.trim_matches(crate::metadata::js_space).to_owned())
                    .filter(|message| !message.is_empty())
                    .map(|message| format!("\n\n{message}"))
                    .unwrap_or_default()
            );
            let digest: JsString = truncate_observer(&digest, 12_000).into();
            let mut observer_request = ForkRequest {
                definition_id: observer.id.clone(),
                directive: digest,
                subagent_id: observer_id.clone(),
                tool_call_id: None,
                timeout_ms: request.timeout_ms,
                context: request.context.clone(),
            };
            let port = self.clone();
            runtime.start(crate::runtime::background::StartBackgroundSubagentInput {
                kind: Some(crate::runtime::background::SubagentKind::Observer),
                session_id: value_string(&self.input, "sessionId"),
                parent_turn_id: value_string(&self.input, "turnId"),
                subagent_id: observer_id.clone(),
                subagent_type: observer.id.clone(),
                observed_subagent_id: Some(observed_id.clone()),
                run: Box::new(move |signal| {
                    Box::pin(async move {
                        observer_request.context = with_signal(&observer_request.context, signal);
                        let started = std::time::Instant::now();
                        let observer_result =
                            Box::pin(port.run(observer_request, observer.clone())).await;
                        let mut event = object_fields([
                            ("type", "observer_report".into()),
                            (
                                "sessionId",
                                JsValue::String(value_string(&port.input, "sessionId")),
                            ),
                            (
                                "turnId",
                                JsValue::String(value_string(&port.input, "turnId")),
                            ),
                            ("observedSubagentId", JsValue::String(observed_id)),
                            ("observedSubagentType", JsValue::String(observed_type)),
                            ("observerSubagentId", JsValue::String(observer_id)),
                            ("observerSubagentType", JsValue::String(observer.id)),
                            ("success", JsValue::Bool(observer_result.is_ok())),
                            (
                                "durationMs",
                                JsValue::Number(started.elapsed().as_secs_f64() * 1000.0),
                            ),
                        ]);
                        match &observer_result {
                            Ok(report) => {
                                event.insert(
                                    "report".into(),
                                    JsValue::String(report.markdown.clone()),
                                );
                            }
                            Err(error) => {
                                event
                                    .insert("error".into(), JsValue::String(error.message.clone()));
                            }
                        }
                        port.events
                            .emit(event)
                            .map_err(|e| JsString::from(e.to_string()))?;
                        observer_result.map(|_| ()).map_err(|e| e.message)
                    })
                }),
            });
        }
        result
    }
}

fn install_definition_skill_scope(
    definition: &Definition,
    registry: &ToolRegistry,
    resources: Option<&super::context_resources::ContextResources>,
) -> Result<(), HostError> {
    if definition.skills.is_some()
        && registry.has(&"read_skill".into())
        && let Some(resources) = resources
    {
        resources.install_scoped_read_skill(registry)?;
    }
    Ok(())
}

fn prepare_observer(mut observer: Definition) -> Definition {
    observer.allowed.clear();
    observer.denied.clear();
    observer.mcp = None;
    observer.skills = None;
    observer.memory_disabled = true;
    observer.read_only = true;
    observer.permission = Some("plan".into());
    observer.initial = None;
    observer.background = false;
    observer.observer = None;
    observer.observer_message = None;
    observer
}

impl OneShotSubagent for NativeSubagentPort {
    fn create_fork(&self, _: JsObject, _: SharedFileState, _: SharedFileState) -> RuntimeContext {
        Arc::new(SubagentForkApi(Arc::new(self.clone())))
    }
}
impl SubagentFork for NativeSubagentPort {
    fn definitions(&self) -> Vec<JsString> {
        NativeSubagentPort::definitions(self)
            .into_iter()
            .map(|d| d.id)
            .collect()
    }
    fn is_allowed_definition(&self, id: &JsString) -> bool {
        self.definition(id).is_some()
    }
    fn depth(&self) -> f64 {
        self.config()
            .get("subagentDepth")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0)
    }
    fn max_depth(&self) -> f64 {
        self.config()
            .get("maxSubagentDepth")
            .and_then(JsValue::as_number)
            .unwrap_or(1.0)
    }
    fn is_background(&self, id: &JsString) -> bool {
        self.definition(id)
            .is_some_and(|definition| definition.background)
    }
    fn has_background_launcher(&self) -> bool {
        self.services.is_some()
    }
    fn fork(&self, request: ForkRequest) -> ToolFuture<ForkReport> {
        let port = self.clone();
        let definition = self.definition(&request.definition_id);
        Box::pin(async move {
            let definition = definition.ok_or_else(|| {
                ToolError::failed(format!(
                    "Unknown subagent type: {}",
                    text(&request.definition_id)
                ))
            })?;
            port.run(request, definition).await
        })
    }
    fn launch_background(&self, mut request: ForkRequest) -> ToolFuture<JsString> {
        let port = self.clone();
        Box::pin(async move {
            let definition = port
                .definition(&request.definition_id)
                .filter(|definition| definition.background)
                .ok_or_else(|| {
                    ToolError::failed(format!(
                        "Subagent type {} is not configured for background execution.",
                        text(&request.definition_id)
                    ))
                })?;
            let runtime = port.services.as_ref().map(|s| s.background.subagents.clone()).ok_or_else(|| ToolError::new("unsupported_tool", format!("Background subagent {} requires a Gateway background-subagent launcher.", text(&request.definition_id))))?;
            let task = runtime.start(crate::runtime::background::StartBackgroundSubagentInput {
                kind: None,
                session_id: value_string(&port.input, "sessionId"),
                parent_turn_id: value_string(&port.input, "turnId"),
                subagent_id: request.subagent_id.clone(),
                subagent_type: definition.id,
                observed_subagent_id: None,
                run: Box::new(move |signal| {
                    Box::pin(async move {
                        request.context = with_signal(&request.context, signal);
                        port.fork(request).await.map(|_| ()).map_err(|e| e.message)
                    })
                }),
            });
            Ok(task.task_id)
        })
    }
}

struct NativeSpawner;
impl super::sidecar_session::SidecarSpawner for NativeSpawner {
    fn command(&self) -> std::io::Result<tokio::process::Command> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Native subagent forks use the embedded session worker.",
        ))
    }
}

struct DefinitionMcp {
    // An owned runtime always travels with its construction-time executor.
    runtime: Option<(
        Arc<dyn crate::runtime::mcp::runtime::McpRuntimePort>,
        tokio::runtime::Handle,
    )>,
}
impl DefinitionMcp {
    async fn open(definition: &Definition, registry: &ToolRegistry) -> Result<Self, ToolError> {
        use crate::runtime::mcp::{
            bridge::{CreateToolDefinitionsOptions, create_mcp_tool_definitions_from_runtime},
            protocol::{McpServerSpec, McpTransportSpec},
            runtime::{McpRuntime, McpRuntimeOptions, McpRuntimePort},
        };
        let mut specs = Vec::new();
        if let Some(servers) = &definition.mcp {
            for (id, config) in servers.entries() {
                let pairs = |key: &str| -> Option<Vec<(String, String)>> {
                    config.get(key).and_then(JsValue::as_object).map(|values| {
                        values
                            .entries()
                            .iter()
                            .map(|(key, value)| {
                                (text(key), value.as_string().map(text).unwrap_or_default())
                            })
                            .collect()
                    })
                };
                let transport = match config
                    .get("type")
                    .and_then(JsValue::as_string)
                    .map(text)
                    .as_deref()
                {
                    Some("stdio") => McpTransportSpec::Stdio {
                        command: text(&value_string(config, "command")),
                        args: config.get("args").and_then(|args| match args {
                            JsValue::Array(args) => Some(
                                args.iter()
                                    .filter_map(JsValue::as_string)
                                    .map(text)
                                    .collect(),
                            ),
                            _ => None,
                        }),
                        env: pairs("env"),
                        cwd: config
                            .get("cwd")
                            .and_then(JsValue::as_string)
                            .filter(|cwd| !cwd.is_empty())
                            .map(text),
                        per_session: false,
                    },
                    Some("sse") => McpTransportSpec::Sse {
                        url: text(&value_string(config, "url")),
                        headers: pairs("headers"),
                    },
                    _ => McpTransportSpec::StreamableHttp {
                        url: text(&value_string(config, "url")),
                        headers: pairs("headers"),
                    },
                };
                specs.push(McpServerSpec {
                    id: text(id),
                    transport,
                    call_timeout_ms: config.get("timeout").and_then(JsValue::as_number),
                });
            }
        }
        if specs.is_empty() {
            return Ok(Self { runtime: None });
        }
        let runtime: Arc<dyn McpRuntimePort> =
            Arc::new(McpRuntime::new(specs, McpRuntimeOptions::default()));
        let mut owned = Self {
            runtime: Some((runtime.clone(), tokio::runtime::Handle::current())),
        };
        let attached = async {
            runtime.start().await.map_err(failed)?;
            for tool in create_mcp_tool_definitions_from_runtime(
                runtime,
                CreateToolDefinitionsOptions::default(),
            )
            .await
            .map_err(failed)?
            {
                let name = tool
                    .fields()
                    .get("name")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                if (definition.allowed.iter().any(|v| v.equals_str("*"))
                    || definition.allowed.contains(&name))
                    && !definition.denied.contains(&name)
                {
                    registry.register_or_replace(tool).map_err(failed)?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = attached {
            if let Err(cleanup) = owned.dispose().await {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "DefinitionMcp startup cleanup failed: {cleanup:?}"
                );
            }
            return Err(error);
        }
        Ok(owned)
    }
    async fn dispose(&mut self) -> Result<(), ToolError> {
        match self.runtime.take() {
            Some((runtime, _)) => runtime.stop().await.map_err(failed),
            None => Ok(()),
        }
    }
}
impl Drop for DefinitionMcp {
    fn drop(&mut self) {
        if let Some((runtime, executor)) = self.runtime.take() {
            drop(crate::runtime::manager::session_mcp::spawn_drop_cleanup(
                executor,
                "DefinitionMcp",
                async move { runtime.stop().await },
            ));
        }
    }
}
struct ChildGuard {
    session: Option<Arc<dyn LiveSession>>,
    // Captured with the child; Drop may run on a plain parent thread.
    executor: tokio::runtime::Handle,
    live: Option<Arc<LiveSessionState>>,
    port: NativeSubagentPort,
    subagent_id: JsString,
    subagent_type: JsString,
    started_recorded: bool,
    lifecycle_started: bool,
    event_started: bool,
    completed_recorded: bool,
    lifecycle_stopped: bool,
    event_completed: bool,
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let mut failures = Vec::new();
            if let Err(error) = session.abort(&"subagent_parent_dropped".into()) {
                failures.push(format!("abort: {error}"));
            }
            let live = self.live.clone();
            let port = self.port.clone();
            let id = self.subagent_id.clone();
            let kind = self.subagent_type.clone();
            let record_completion = self.started_recorded && !self.completed_recorded;
            let emit_completion = self.event_started && !self.event_completed;
            let stop_lifecycle = self.lifecycle_started && !self.lifecycle_stopped;
            drop(crate::runtime::manager::session_mcp::spawn_drop_cleanup(
                self.executor.clone(),
                "ChildGuard",
                async move {
                    if let Err(error) = session.dispose().await {
                        failures.push(format!("session dispose: {error}"));
                    }
                    if let Some(live) = live {
                        if let Err(error) = live.dispose() {
                            failures.push(format!("live dispose: {error}"));
                        }
                    }
                    let writer = port
                        .live
                        .as_ref()
                        .map(|s| s.recorder.writer())
                        .or_else(|| port.services.as_ref().map(|s| &s.writer));
                    if let Some(writer) = writer.filter(|_| record_completion) {
                        if let Err(error) = writer.record_session_event(
                            &value_string(&port.input, "sessionId"),
                            &value_string(&port.input, "turnId"),
                            object_fields([
                                ("type", "subagent_completed".into()),
                                ("subagentId", JsValue::String(id.clone())),
                                ("subagentType", JsValue::String(kind.clone())),
                                (
                                    "summaryPreview",
                                    "Subagent aborted before completion.".into(),
                                ),
                                ("summaryTruncated", JsValue::Bool(false)),
                                ("turns", JsValue::Number(0.0)),
                                ("durationMs", JsValue::Number(0.0)),
                                ("errored", JsValue::Bool(true)),
                            ]),
                        ) {
                            failures.push(format!("completion record: {error}"));
                        }
                    }
                    if stop_lifecycle {
                        let config = port.config();
                        if let Err(error) = lifecycle::Handler::new(
                            port.lifecycle.clone(),
                            lifecycle::Binding {
                                session_id: value_string(&port.input, "sessionId"),
                                turn_id: value_string(&port.input, "turnId"),
                                cwd: string_field(&config, "cwd"),
                                permission_mode: config
                                    .get("permissionMode")
                                    .and_then(JsValue::as_string)
                                    .cloned(),
                                base_env: config.get("env").and_then(JsValue::as_object).cloned(),
                                signal: None,
                            },
                        )
                        .call(object([(
                            "payload",
                            object([
                                ("operation", "dispatch".into()),
                                ("event", "SubagentStop".into()),
                                (
                                    "payload",
                                    object([
                                        ("subagentId", JsValue::String(id.clone())),
                                        ("subagentType", JsValue::String(kind.clone())),
                                        ("success", JsValue::Bool(false)),
                                    ]),
                                ),
                            ]),
                        )]))
                        .await
                        {
                            failures.push(format!("stop lifecycle: {error}"));
                        }
                    }
                    if emit_completion {
                        if let Err(error) = port.events.emit(object_fields([
                            ("type", "subagent_completed".into()),
                            (
                                "sessionId",
                                JsValue::String(value_string(&port.input, "sessionId")),
                            ),
                            (
                                "turnId",
                                JsValue::String(value_string(&port.input, "turnId")),
                            ),
                            ("subagentId", JsValue::String(id)),
                            ("subagentType", JsValue::String(kind)),
                            ("success", JsValue::Bool(false)),
                            ("aborted", JsValue::Bool(true)),
                            ("durationMs", JsValue::Number(0.0)),
                        ])) {
                            failures.push(format!("completion event: {error}"));
                        }
                    }
                    if failures.is_empty() {
                        Ok(())
                    } else {
                        Err(crate::runtime::project::RuntimeError::new(
                            failures.join("; "),
                        ))
                    }
                },
            ));
        }
    }
}

#[derive(Clone)]
struct Definition {
    key: JsString,
    id: JsString,
    allowed: Vec<JsString>,
    denied: Vec<JsString>,
    read_only: bool,
    omit_project: bool,
    omit_git: bool,
    prompt: JsString,
    initial: Option<JsString>,
    model: Option<JsObject>,
    max_turns: Option<f64>,
    permission: Option<JsString>,
    background: bool,
    observer: Option<JsString>,
    observer_message: Option<JsString>,
    reminder: Option<JsString>,
    mcp: Option<JsObject>,
    skills: Option<Vec<String>>,
    memory_disabled: bool,
}
impl Definition {
    fn builtin(id: &str) -> Self {
        let read_only = id != "general-purpose";
        let allowed = match id {
            "general-purpose" => vec!["*"],
            "plan" => vec!["read_file", "grep", "glob"],
            _ => vec!["read_file", "grep", "glob", "bash"],
        }
        .into_iter()
        .map(JsString::from)
        .collect();
        let suffix = match id {
            "general-purpose" => {
                "You have full access to the parent's tool registry. Use any tool the parent has permission to use, but stay within your directive."
            }
            "plan" => {
                "Planning mode: produce a numbered, actionable plan. You may inspect code (`read_file` / `grep` / `glob`) but you may NOT execute shell commands or modify files. Prefer `grep` for content search and `glob` for file discovery instead of describing shell-based search workarounds."
            }
            "verify" => {
                r"Verification mode: your job is to **find problems**, not confirm success. Try to break the implementation.

Approach:
1. Read and inspect the generated artifacts (files, images, screenshots, HTML pages).
2. Run validation commands (e.g. check file sizes, run linters, verify encoding).
3. For images: use `read_file` on image files to visually inspect them (if the model supports multimodal input). Look for: missing/garbled text (tofu boxes), layout overflow, color contrast issues, truncated content.
4. For HTML: check that the file is well-formed, that links and assets resolve, and that i18n/RTL is correct.
5. For data files: verify schema, required fields, encoding (UTF-8), and sanity of values.

Output your findings as a structured verdict:
- PASS: all checks passed, no issues found.
- PARTIAL: some checks passed but minor issues exist (list them).
- FAIL: critical issues found (list them with file paths and descriptions).

Be rigorous. A silent pass when issues exist is worse than a false alarm."
            }
            _ => {
                "Read-only mode: you may inspect files (`read_file`), search (`grep` / `glob`), and run safe shell commands (`bash`), but never write or delete. Prefer `grep` for content search and `glob` for file discovery; do not fall back to `bash` `grep` / `rg` unless the dedicated tools cannot express the task. Do not propose `edit_file` / `write_file` / network calls — those will be rejected."
            }
        };
        Self {
            key: id.into(),
            id: id.into(),
            allowed,
            denied: vec![],
            read_only,
            omit_project: read_only,
            omit_git: read_only,
            prompt: suffix.into(),
            initial: None,
            model: None,
            max_turns: None,
            permission: None,
            background: false,
            observer: None,
            observer_message: None,
            reminder: None,
            mcp: None,
            skills: None,
            memory_disabled: false,
        }
    }
    fn parse(id: &JsString, value: &JsValue) -> Self {
        let strings = |key| match value.get(key) {
            Some(JsValue::Array(items)) => items
                .iter()
                .filter_map(JsValue::as_string)
                .cloned()
                .collect(),
            _ => vec![],
        };
        Self {
            key: id.clone(),
            id: value
                .get("id")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| id.clone()),
            allowed: strings("allowedTools"),
            denied: strings("disallowedTools"),
            read_only: value
                .get("isReadOnly")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
            omit_project: value
                .get("omitProjectInstructions")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
            omit_git: value
                .get("omitGitStatus")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
            prompt: value_string(value, "systemPromptSuffix"),
            initial: value
                .get("initialPrompt")
                .and_then(JsValue::as_string)
                .cloned(),
            model: value
                .get("modelOverride")
                .and_then(JsValue::as_object)
                .cloned(),
            max_turns: value.get("maxTurns").and_then(JsValue::as_number),
            permission: value
                .get("permissionMode")
                .and_then(JsValue::as_string)
                .cloned(),
            background: value.get("background").and_then(JsValue::as_bool) == Some(true),
            observer: value.get("observer").and_then(JsValue::as_string).cloned(),
            observer_message: value
                .get("observerMessage")
                .and_then(JsValue::as_string)
                .cloned(),
            reminder: value
                .get("criticalSystemReminder")
                .and_then(JsValue::as_string)
                .cloned(),
            mcp: value
                .get("mcpServers")
                .and_then(JsValue::as_object)
                .cloned(),
            skills: match value.get("skills") {
                Some(JsValue::Array(values)) => Some(
                    values
                        .iter()
                        .filter_map(JsValue::as_string)
                        .map(|value| String::from_utf16_lossy(value.units()))
                        .collect(),
                ),
                _ => None,
            },
            memory_disabled: value
                .get("memory")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str("disabled")),
        }
    }
    fn config(&self, parent: &JsObject, id: &JsString) -> JsObject {
        let mut result = JsObject::new();
        let baseline = parent.get("subagentModel").and_then(JsValue::as_object);
        let selected = self
            .model
            .as_ref()
            .map(|explicit| {
                let mut selected = baseline
                    .filter(|base| {
                        base.get("provider") == explicit.get("provider")
                            && base.get("model") == explicit.get("model")
                    })
                    .cloned()
                    .unwrap_or_default();
                for (key, value) in explicit.entries() {
                    selected.insert(key.clone(), value.clone());
                }
                selected
            })
            .or_else(|| baseline.cloned());
        for (key, value) in parent.entries() {
            if selected.is_some()
                && (key.equals_str("maxContextTokens") || key.equals_str("maxOutputTokens"))
            {
                continue;
            }
            result.insert(key.clone(), value.clone());
        }
        if let Some(model) = &selected {
            for key in ["provider", "model", "modelMultimodal"] {
                if let Some(value) = model.get(key) {
                    result.insert(key.into(), value.clone());
                }
            }
        }
        if self.read_only
            || parent.get("permissionMode") == Some(&"plan".into())
            || parent.get("runMode") == Some(&"ask".into())
        {
            result.insert("runMode".into(), "ask".into());
        }
        if let Some(mode) = &self.permission {
            result.insert("permissionMode".into(), JsValue::String(mode.clone()));
            let mut permission = parent
                .get("permissionContext")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            permission.insert("mode".into(), JsValue::String(mode.clone()));
            result.insert("permissionContext".into(), JsValue::Object(permission));
        }
        result.insert("isSubagent".into(), JsValue::Bool(true));
        result.insert("stopOnStructuredOutput".into(), JsValue::Bool(false));
        // The baseline one-shot adapter and composition both increment depth.
        result.insert(
            "subagentDepth".into(),
            JsValue::Number(
                parent
                    .get("subagentDepth")
                    .and_then(JsValue::as_number)
                    .unwrap_or(0.0)
                    + 2.0,
            ),
        );
        result.insert(
            "omitProjectInstructions".into(),
            JsValue::Bool(self.omit_project),
        );
        let mut inherited = text(&string_field(parent, "systemPrompt"));
        if self.omit_project {
            inherited = strip_blocks(&inherited, &["agents-md", "project-instructions"]);
        }
        if self.omit_git {
            inherited = strip_blocks(&inherited, &["git-status", "gitStatus"]);
        }
        result.insert(
            "systemPrompt".into(),
            JsValue::String(
                format!(
                    "{SHARED_PROMPT}\n\n{}{}{}",
                    text(&self.prompt),
                    self.reminder
                        .as_ref()
                        .map(|r| format!("\n\n{}", text(r)))
                        .unwrap_or_default(),
                    if inherited.trim_end().is_empty() {
                        String::new()
                    } else {
                        format!("\n\n{}", inherited.trim_end())
                    }
                )
                .into(),
            ),
        );
        let mut metadata = parent
            .get("metadata")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap_or_default();
        metadata.insert("subagentId".into(), JsValue::String(id.clone()));
        metadata.insert("subagentType".into(), JsValue::String(self.id.clone()));
        result.insert("metadata".into(), JsValue::Object(metadata));
        result
    }
}

const SHARED_PROMPT: &str = r#"You are a subagent of PilotDeck — a focused agent dispatched by the parent agent to handle a bounded research, planning, or verification task.

Strengths:
- You always have the full context of the parent task and can inspect the parent's tool history.
- You return a single concise final report (no follow-up questions).
- You never ask clarifying questions back; do your best with the information given.

Guidelines:
1. Stay strictly within the directive given by the parent.
2. Do NOT create files unless the directive explicitly asks you to.
3. NEVER proactively create documentation or README files.
4. Run only the tools listed in your allowed tool set; never attempt restricted ones.
5. If the directive asks you to write/save files, do that BEFORE producing your final report.
   File writing is part of the task, not optional.
6. The final assistant message MUST follow the output format below verbatim.
7. Keep the response under 4 KB unless the directive demands more.
8. Use absolute paths when referencing files.
9. Trust the parent's directive: do not re-question its premises.
10. If the directive is impossible with the allowed tools, say so explicitly in the report.
11. Prefer fewer tool calls. Do not use web_fetch unless the directive explicitly requires full page content.
12. When the directive provides specific file paths, trust them and use them directly. Do not spend turns searching for or verifying file paths that are already given.

Output format (mandatory; missing any field fails the run):
Scope: <one sentence describing what you did>
Result: <findings, in markdown if helpful>
Key files: <comma-separated absolute paths or "none">
Files changed: <list with rationale, or "none">
Issues: <list of caveats / blockers, or "none">"#;

fn strip_blocks(input: &str, tags: &[&str]) -> String {
    let mut value = input.to_owned();
    for tag in tags {
        let pattern = format!(
            r"(?:^|\n)\s*<{}(?:\s[^>]*)?>[\s\S]*?</{}>\s*",
            regex::escape(tag),
            regex::escape(tag)
        );
        if let Ok(regex) = regex::Regex::new(&pattern) {
            value = regex.replace_all(&value, "\n").into_owned();
        }
    }
    value
}
fn parse_summary(value: &JsString) -> Option<JsObject> {
    let text = text(value);
    let lines = text.split('\n').collect::<Vec<_>>();
    let fields = ["Scope", "Result", "Key files", "Files changed", "Issues"];
    let mut result = JsObject::new();
    for field in fields {
        let prefix = format!("{field}:");
        let index = lines.iter().position(|line| line.starts_with(&prefix))?;
        let mut value = lines[index][prefix.len()..]
            .trim_matches(crate::metadata::js_space)
            .to_owned();
        for line in &lines[index + 1..] {
            if fields.iter().any(|f| line.starts_with(&format!("{f}:"))) {
                break;
            }
            value.push('\n');
            value.push_str(line);
        }
        result.insert(
            field.into(),
            value.trim_matches(crate::metadata::js_space).into(),
        );
    }
    Some(result)
}
fn assistant_text(value: &JsValue) -> JsString {
    let Some(JsValue::Array(content)) = value.get("content") else {
        return JsString::default();
    };
    content
        .iter()
        .filter(|b| b.get("type") == Some(&"text".into()))
        .filter_map(|b| b.get("text").and_then(JsValue::as_string))
        .map(text)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches(crate::metadata::js_space)
        .into()
}
fn message(value: JsString) -> JsValue {
    object([
        ("role", "user".into()),
        (
            "content",
            JsValue::Array(vec![object([
                ("type", "text".into()),
                ("text", JsValue::String(value)),
            ])]),
        ),
    ])
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn value_string(value: &JsValue, key: &str) -> JsString {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}
fn string_field(value: &JsObject, key: &str) -> JsString {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}
fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::failed(error.to_string())
}
fn activity_line(event: &JsValue) -> Option<String> {
    let kind = value_string(event, "type");
    match text(&kind).as_str() {
        "model_request_started" => Some(format!(
            "model request: {}/{}",
            text(&value_string(event, "provider")),
            text(&value_string(event, "model"))
        )),
        "tool_calls_detected" => Some(format!(
            "tool calls: {}",
            match event.get("calls") {
                Some(JsValue::Array(items)) if !items.is_empty() => items
                    .iter()
                    .map(|v| text(&value_string(v, "name")))
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => "none".into(),
            }
        )),
        "tool_result" => Some(format!(
            "tool result: {} ({})",
            text(&value_string(event.get("result")?, "toolName")),
            if event.get("result")?.get("type") == Some(&"success".into()) {
                "success"
            } else {
                "error"
            }
        )),
        "assistant_message" => Some("assistant response emitted".into()),
        "warning" => Some(format!(
            "warning: {}",
            truncate_observer(&text(&value_string(event, "code")), 120)
        )),
        "agent_status" => Some(format!(
            "agent status: {}",
            truncate_observer(&text(&value_string(event, "event")), 120)
        )),
        _ => None,
    }
}
fn truncate_observer(value: &str, limit: usize) -> String {
    let value = value.trim_matches(crate::metadata::js_space);
    if value.encode_utf16().count() <= limit {
        value.to_owned()
    } else {
        format!(
            "{}\n\n[observer digest truncated]",
            String::from_utf16_lossy(
                &value
                    .encode_utf16()
                    .take(limit.saturating_sub(72))
                    .collect::<Vec<_>>()
            )
        )
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        _ => true,
    }
}

fn has_text(value: &JsValue) -> bool {
    matches!(value.get("content"), Some(JsValue::Array(items)) if items.iter().any(|b| b.get("type") == Some(&"text".into())))
}
fn object_fields<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in entries {
        result.insert(key.into(), value);
    }
    result
}
fn preview(value: &JsString, limit: usize) -> (JsString, bool) {
    let source = text(value);
    if source.len() <= limit {
        return (value.clone(), false);
    }
    let mut end = 0;
    for (index, ch) in source.char_indices() {
        if index + ch.len_utf8() > limit {
            break;
        }
        end = index + ch.len_utf8();
    }
    (source[..end].into(), true)
}

fn with_signal(
    context: &crate::runtime::tools::ToolRunContext,
    signal: crate::runtime::tools::ToolCancellation,
) -> crate::runtime::tools::ToolRunContext {
    use crate::host::modules::tool_context::ToolRuntimeContext;
    let host = &context.host;
    crate::runtime::tools::ToolRunContext {
        fields: context.fields.clone(),
        host: Arc::new(ToolRuntimeContext {
            fields: host.fields.clone(),
            files: host.files.clone(),
            abort_signal: Some(Arc::new(signal)),
            model: host.model.clone(),
            elicitation: host.elicitation.clone(),
            user_dialog: host.user_dialog.clone(),
            audit_recorder: host.audit_recorder.clone(),
            file_history: host.file_history.clone(),
            file_update_notifier: host.file_update_notifier.clone(),
            now: host.now.clone(),
            progress: host.progress.clone(),
            subagent: host.subagent.clone(),
            plan_todo: host.plan_todo.clone(),
            goal: host.goal.clone(),
            plan_directory: None,
        }),
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;
    use crate::runtime::mcp::{
        client::{McpCallOptions, McpClientError},
        protocol::McpToolSpec,
        runtime::{
            McpClientStatusEntry, McpFuture, McpRuntime, McpRuntimeOptions, McpRuntimePort,
            McpRuntimeServerInfo, McpToolCallResult, SessionMcpRuntime,
        },
    };
    use crate::runtime::project::{DisposeFuture, RuntimeError};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    struct StopProbe {
        stopped: mpsc::Sender<()>,
        runtime: McpRuntime,
    }
    impl SessionMcpRuntime for StopProbe {
        fn stop(&self) -> McpFuture<'_, Result<(), RuntimeError>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                self.stopped.send(()).unwrap();
                Ok(())
            })
        }
    }
    impl McpRuntimePort for StopProbe {
        fn start(&self) -> McpFuture<'_, Result<Vec<McpClientStatusEntry>, RuntimeError>> {
            McpRuntimePort::start(&self.runtime)
        }
        fn list_all_tools(&self) -> McpFuture<'_, Result<Vec<McpToolSpec>, RuntimeError>> {
            McpRuntimePort::list_all_tools(&self.runtime)
        }
        fn server_info(&self, id: &str) -> Option<McpRuntimeServerInfo> {
            self.runtime.server_info(id)
        }
        fn call_tool<'a>(
            &'a self,
            id: &'a str,
            name: &'a JsString,
            input: &'a JsValue,
            options: McpCallOptions,
        ) -> McpFuture<'a, Result<McpToolCallResult, McpClientError>> {
            McpRuntimePort::call_tool(&self.runtime, id, name, input, options)
        }
    }

    #[test]
    fn definition_mcp_drop_without_current_runtime_stops() {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (stopped, observed) = mpsc::channel();
        let owned = DefinitionMcp {
            runtime: Some((
                Arc::new(StopProbe {
                    stopped,
                    runtime: McpRuntime::new(Vec::new(), McpRuntimeOptions::default()),
                }),
                executor.handle().clone(),
            )),
        };
        std::thread::spawn(move || {
            assert!(tokio::runtime::Handle::try_current().is_err());
            drop(owned);
        })
        .join()
        .unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    struct ChildProbe {
        aborted: Arc<AtomicUsize>,
        disposed: mpsc::Sender<()>,
        fail: bool,
    }
    impl LiveSession for ChildProbe {
        fn snapshot(&self) -> JsValue {
            JsValue::Null
        }
        fn abort(&self, reason: &JsString) -> Result<(), RuntimeError> {
            assert!(reason.equals_str("subagent_parent_dropped"));
            self.aborted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                self.disposed.send(()).unwrap();
                if self.fail {
                    Err(RuntimeError::new("child dispose probe error"))
                } else {
                    Ok(())
                }
            })
        }
    }

    #[test]
    fn child_guard_drop_without_current_runtime_disposes_and_emits_even_on_error() {
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        for fail in [false, true] {
            let aborted = Arc::new(AtomicUsize::new(0));
            let (disposed, observed) = mpsc::channel();
            let (event, emitted) = mpsc::channel();
            let events = Arc::new(module_host::EventBuffer::streaming(Arc::new(
                move |value| {
                    event.send(value).unwrap();
                    Ok(())
                },
            )));
            let port = NativeSubagentPort::new(
                JsValue::Undefined,
                object([("sessionId", "parent".into()), ("turnId", "turn".into())]),
                ToolRegistry::default(),
                events,
                None,
                None,
                None,
                None,
                crate::runtime::hooks::LifecycleRuntime::null(),
            );
            let guard = ChildGuard {
                session: Some(Arc::new(ChildProbe {
                    aborted: aborted.clone(),
                    disposed,
                    fail,
                })),
                executor: executor.handle().clone(),
                live: None,
                port,
                subagent_id: "child".into(),
                subagent_type: "worker".into(),
                started_recorded: false,
                lifecycle_started: false,
                event_started: true,
                completed_recorded: false,
                lifecycle_stopped: false,
                event_completed: false,
            };
            std::thread::spawn(move || {
                assert!(tokio::runtime::Handle::try_current().is_err());
                drop(guard);
            })
            .join()
            .unwrap();
            observed.recv_timeout(Duration::from_secs(2)).unwrap();
            let event = emitted.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(event.get("type"), Some(&"subagent_completed".into()));
            assert_eq!(event.get("subagentId"), Some(&"child".into()));
            assert_eq!(aborted.load(Ordering::SeqCst), 1);
        }
    }
}

#[cfg(test)]
mod organization_limit_tests {
    use super::*;
    use crate::runtime::context::memory::{
        MemoryCaptureFuture, MemoryCaptureTurnInput, MemoryFuture, MemoryResolver,
        MemoryRetrieveInput, MemoryRetrieveResult,
    };
    use crate::runtime::plugins::{PluginSessionContributionSnapshot, PluginSkillContribution};

    struct Memory;
    impl MemoryResolver for Memory {
        fn retrieve(&self, _: MemoryRetrieveInput) -> MemoryFuture {
            Box::pin(async { Ok(MemoryRetrieveResult::default()) })
        }

        fn capture_turn(&self, _: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
            Box::pin(async { Ok(()) })
        }
    }

    fn value(json: &str) -> JsValue {
        pilotdeck_protocol::parse_json(json).unwrap()
    }

    #[test]
    fn organization_caps_agent_turns_depth_and_timeout() {
        let sdk = value(
            r#"{
                "agents": {
                    "explicit": {"description":"x","prompt":"x","maxTurns":10},
                    "defaulted": {"description":"x","prompt":"x"}
                },
                "settings":{"agent":{"subagents":{"maxDepth":8}}}
            }"#,
        );
        let mut agent = JsObject::new();
        let policy = crate::runtime::sdk_session::OrganizationPolicy {
            max_turns: Some(4.0),
            max_subagent_depth: Some(2.0),
            max_subagent_timeout_ms: Some(1_000.0),
            ..Default::default()
        };
        apply_sdk_config_with_policy(&mut agent, Some(&sdk), None, Some(&policy));
        let definitions = agent
            .get("subagentDefinitions")
            .and_then(JsValue::as_object)
            .unwrap();
        assert_eq!(
            definitions.get("explicit").and_then(|v| v.get("maxTurns")),
            Some(&JsValue::Number(4.0))
        );
        assert_eq!(
            definitions.get("defaulted").and_then(|v| v.get("maxTurns")),
            Some(&JsValue::Number(4.0))
        );
        assert_eq!(agent.get("maxSubagentDepth"), Some(&JsValue::Number(2.0)));
        assert_eq!(
            agent.get("subagentTimeoutMs"),
            Some(&JsValue::Number(1_000.0))
        );
    }

    #[test]
    fn absent_organization_caps_preserve_unconfigured_defaults() {
        let sdk = value(
            r#"{"agents":{"defaulted":{"description":"x","prompt":"x"}},"settings":{"agent":{"subagents":{}}}}"#,
        );
        let mut agent = JsObject::new();
        apply_sdk_config_with_policy(&mut agent, Some(&sdk), None, None);
        let definition = agent
            .get("subagentDefinitions")
            .and_then(JsValue::as_object)
            .and_then(|definitions| definitions.get("defaulted"))
            .unwrap();
        assert!(definition.get("maxTurns").is_none());
        assert!(agent.get("maxSubagentDepth").is_none());
        assert!(agent.get("subagentTimeoutMs").is_none());
    }

    /// TS `SessionAgentConfigBundle.ts:223-225` maxTurns cap,
    /// `:148-151` depth/timeout caps, `capSubagentDepth` depth=0.
    #[test]
    fn organization_caps_min_definition_and_host_limits() {
        let sdk = value(
            r#"{
                "agents": {
                    "worker": {"description":"d","prompt":"p","maxTurns":99}
                },
                "settings": {"agent": {"subagents": {
                    "maxDepth": 8,
                    "timeoutMs": 60000
                }}}
            }"#,
        );
        let mut agent = JsObject::new();
        let policy = crate::runtime::sdk_session::OrganizationPolicy {
            max_turns: Some(4.0),
            max_subagent_depth: Some(0.0),
            max_subagent_timeout_ms: Some(900.0),
            ..Default::default()
        };
        apply_sdk_config_with_policy(&mut agent, Some(&sdk), None, Some(&policy));
        let worker = agent
            .get("subagentDefinitions")
            .and_then(JsValue::as_object)
            .and_then(|d| d.get("worker"))
            .unwrap();
        assert_eq!(worker.get("maxTurns"), Some(&JsValue::Number(4.0)));
        assert_eq!(agent.get("maxSubagentDepth"), Some(&JsValue::Number(0.0)));
        assert_eq!(
            agent.get("subagentTimeoutMs"),
            Some(&JsValue::Number(900.0))
        );
    }

    #[test]
    fn observer_always_clears_skills_and_memory() {
        let definition = Definition::parse(
            &"observer".into(),
            &value(
                r#"{"skills":["review"],"memory":"inherit","observer":"nested","observerMessage":"note"}"#,
            ),
        );
        let observer = prepare_observer(definition);
        assert!(observer.skills.is_none());
        assert!(observer.memory_disabled);
        assert!(observer.observer.is_none());
        assert!(observer.observer_message.is_none());
    }

    #[test]
    fn module_host_resources_reach_subagent_and_follow_memory_mode() {
        let resources = Arc::new(
            super::super::context_resources::ContextResources::empty("/project")
                .with_memory_resolver(Some(Arc::new(Memory))),
        );
        let port = module_host::with_context_resources(resources, || {
            NativeSubagentPort::new(
                JsValue::Undefined,
                JsValue::Object(JsObject::new()),
                ToolRegistry::default(),
                Arc::new(module_host::EventBuffer::default()),
                None,
                None,
                None,
                None,
                crate::runtime::hooks::LifecycleRuntime::null(),
            )
        });
        let captured = port.context_resources.as_ref().unwrap();
        assert!(captured.memory_resolver.is_some());

        let inherited = Definition::parse(&"child".into(), &value(r#"{"memory":"inherit"}"#));
        assert!(
            captured
                .for_subagent(inherited.skills.as_deref(), inherited.memory_disabled)
                .memory_resolver
                .is_some()
        );
        let disabled = Definition::parse(&"child".into(), &value(r#"{"memory":"disabled"}"#));
        assert!(
            captured
                .for_subagent(disabled.skills.as_deref(), disabled.memory_disabled)
                .memory_resolver
                .is_none()
        );
    }

    #[test]
    fn definition_skills_replace_only_an_allowed_existing_read_skill() {
        let resources = super::super::context_resources::ContextResources::from_contributions(
            PluginSessionContributionSnapshot {
                generation: 1,
                tools: vec![],
                routers: vec![],
                commands: vec![],
                skills: vec![PluginSkillContribution {
                    name: "review".into(),
                    description: None,
                    path: "/skills/review/SKILL.md".into(),
                    content: "review".into(),
                    namespace: "test".into(),
                }],
                prompts: vec![],
                hooks: Default::default(),
                mcp_servers: JsObject::new(),
                mcp_instructions: vec![],
            },
            "/project",
        );
        let parent = ToolRegistry::default();
        resources.install_read_skill(&parent).unwrap();
        let parent_tool = parent.get(&"read_skill".into()).unwrap();

        let inherited_definition = Definition::parse(&"child".into(), &value("{}"));
        let inherited_registry = parent.scoped(Arc::new(|_| true)).unwrap();
        let inherited_resources = resources.for_subagent(None, false);
        install_definition_skill_scope(
            &inherited_definition,
            &inherited_registry,
            Some(&inherited_resources),
        )
        .unwrap();
        assert!(Arc::ptr_eq(
            &parent_tool,
            &inherited_registry.get(&"read_skill".into()).unwrap()
        ));

        let empty_definition = Definition::parse(
            &"child".into(),
            &value(r#"{"skills":[],"allowedTools":["read_skill"]}"#),
        );
        let empty_registry = parent.scoped(Arc::new(|_| true)).unwrap();
        let empty_resources = resources.for_subagent(Some(&[]), false);
        install_definition_skill_scope(&empty_definition, &empty_registry, Some(&empty_resources))
            .unwrap();
        assert!(!Arc::ptr_eq(
            &parent_tool,
            &empty_registry.get(&"read_skill".into()).unwrap()
        ));
        assert!(empty_resources.extension().list_skills().is_empty());

        let blocked_registry = parent.scoped(Arc::new(|_| false)).unwrap();
        install_definition_skill_scope(
            &empty_definition,
            &blocked_registry,
            Some(&empty_resources),
        )
        .unwrap();
        assert!(!blocked_registry.has(&"read_skill".into()));

        let absent_registry = ToolRegistry::default();
        install_definition_skill_scope(&empty_definition, &absent_registry, Some(&empty_resources))
            .unwrap();
        assert!(!absent_registry.has(&"read_skill".into()));
    }
}
