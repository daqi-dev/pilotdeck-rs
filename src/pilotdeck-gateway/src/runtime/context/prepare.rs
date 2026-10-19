//! `prepareForModel` mainline: message projection, prompt-time state, prompt
//! assembly and instruction layering.
//!
//! Ported from `context/DefaultContextRuntime.ts:167-426` without the memory
//! attachment block (A6.6-pr5b) and without prompt-cache generation (cache
//! owner). Prompt contributions are session-owned and injected as a registry.
use super::{
    instructions::{InstructionDiscovery, Layer},
    prompt::{
        ContributionContext, ExtensionResolver, PromptAssembler, PromptContributionRegistry,
        SnapshotOptions, escape_xml_attribute, iso_date, render_prompt_contribution_sections,
        render_prompt_runtime_context_sections,
    },
};
use crate::host::modules::context::{ContextInput, ContextRuntime, Operation};
use crate::host::{Cancellation, HostError, HostFuture, ModuleError};
use crate::metadata::js_trim;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

static UNDEFINED: JsValue = JsValue::Undefined;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeContextSurface {
    #[default]
    SystemPrompt,
    UserMessage,
    /// Any value other than the two known profiles keeps TS's branch behavior:
    /// no user context in the system prompt and no durable user-role message.
    Other,
}
impl RuntimeContextSurface {
    fn parse(value: &JsValue) -> Self {
        if value
            .as_string()
            .is_some_and(|value| value.equals_str("system_prompt"))
        {
            Self::SystemPrompt
        } else if value
            .as_string()
            .is_some_and(|value| value.equals_str("user_message"))
        {
            Self::UserMessage
        } else {
            Self::Other
        }
    }
}

pub struct Options {
    pub extension: Option<Arc<dyn ExtensionResolver>>,
    pub include_extensions_with_custom_system_prompt: bool,
    pub runtime_context_surface: RuntimeContextSurface,
    pub instruction_discovery: Option<InstructionDiscovery>,
    pub prompt_contributions: Option<PromptContributionRegistry>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    /// Host identity rendered in the user-context block. TS reads
    /// `process.platform` / `process.version`; the Rust host has no Node, so
    /// integration supplies these (fixtures inject the recorded values).
    pub platform: Option<String>,
    pub node_version: Option<String>,
    pub locale: Option<String>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            extension: None,
            include_extensions_with_custom_system_prompt: false,
            runtime_context_surface: RuntimeContextSurface::SystemPrompt,
            instruction_discovery: None,
            prompt_contributions: None,
            now: None,
            platform: None,
            node_version: None,
            locale: None,
        }
    }
}

struct Inner {
    assembler: PromptAssembler,
    include_extensions_with_custom_system_prompt: bool,
    runtime_context_surface: RuntimeContextSurface,
    instruction_discovery: Option<InstructionDiscovery>,
    prompt_contributions: Option<PromptContributionRegistry>,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    state: Mutex<HashMap<Option<JsString>, TimeState>>,
}
#[derive(Clone)]
pub struct DefaultContextRuntime {
    inner: Arc<Inner>,
}
impl DefaultContextRuntime {
    pub fn new(options: Options) -> Self {
        let extension = options
            .extension
            .unwrap_or_else(|| Arc::new(super::prompt::NullExtensionResolver));
        let locale = options
            .locale
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| crate::project_files::node_locale(|key| std::env::var(key).ok()));
        let assembler = PromptAssembler::with_environment(
            extension,
            locale,
            options.platform.unwrap_or_else(|| {
                match std::env::consts::OS {
                    "macos" => "darwin",
                    "windows" => "win32",
                    "solaris" => "sunos",
                    other => other,
                }
                .to_owned()
            }),
            options.node_version.unwrap_or_else(|| "v22.22.0".into()),
        );
        Self {
            inner: Arc::new(Inner {
                assembler,
                include_extensions_with_custom_system_prompt: options
                    .include_extensions_with_custom_system_prompt,
                runtime_context_surface: options.runtime_context_surface,
                instruction_discovery: options.instruction_discovery,
                prompt_contributions: options.prompt_contributions,
                now: options.now.unwrap_or_else(|| Arc::new(system_now)),
                state: Mutex::new(HashMap::new()),
            }),
        }
    }
    /// Prompt-time state is session scoped; release it when a session ends.
    pub fn release_session(&self, session_id: &JsString) {
        self.inner
            .state
            .lock()
            .unwrap()
            .remove(&Some(session_id.clone()));
    }

    async fn prepare(
        &self,
        fields: JsObject,
        abort: Option<Cancellation>,
    ) -> Result<JsValue, HostError> {
        let input = PrepareInput::parse(&fields, self.inner.runtime_context_surface)?;
        let Projection {
            messages,
            dropped_count,
            warnings,
        } = project(
            &input.messages,
            input
                .max_messages
                .as_ref()
                .map(|(max, raw)| (*max, raw.as_str())),
        );
        let mut diagnostics: Vec<Diagnostic> = warnings
            .into_iter()
            .map(|warning| Diagnostic {
                code: warning.code.into(),
                severity: "warning",
                message: warning.message,
            })
            .collect();

        let contribution_snapshot = if let Some(registry) = &self.inner.prompt_contributions {
            let context = ContributionContext {
                session_id: input.session_id_text.clone(),
                turn_id: input.turn_id.clone(),
                cwd: input.cwd.clone(),
                provider: input.provider.clone(),
                model: input.model.clone(),
                permission_mode: input.permission_mode.clone(),
                run_mode: input.run_mode.clone(),
                additional_working_directories: input.additional_working_directories.clone(),
                abort,
            };
            let variables = vec![
                ("cwd".to_owned(), contribution_value(fields.get("cwd"))),
                (
                    "provider".to_owned(),
                    contribution_value(fields.get("provider")),
                ),
                ("model".to_owned(), contribution_value(fields.get("model"))),
                (
                    "permission_mode".to_owned(),
                    contribution_value(fields.get("permissionMode")),
                ),
                (
                    "run_mode".to_owned(),
                    contribution_value(fields.get("runMode")),
                ),
                (
                    "session_id".to_owned(),
                    contribution_value(fields.get("sessionId")),
                ),
                (
                    "turn_id".to_owned(),
                    contribution_value(fields.get("turnId")),
                ),
            ];
            Some(
                registry
                    .snapshot(
                        &context,
                        SnapshotOptions {
                            tool_schemas: Some(&input.tools),
                            variables,
                        },
                    )
                    .await
                    .map_err(ModuleError::new)?,
            )
        } else {
            None
        };
        let contributed_sections = match &contribution_snapshot {
            Some(snapshot) => {
                render_prompt_contribution_sections(snapshot).map_err(ModuleError::new)?
            }
            None => Vec::new(),
        };
        let registered_runtime_contexts = match &contribution_snapshot {
            Some(snapshot) => {
                render_prompt_runtime_context_sections(snapshot).map_err(ModuleError::new)?
            }
            None => Vec::new(),
        };
        let has_complete_contribution = contribution_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.sections.iter().any(|section| section.complete));
        let effective_tools = contribution_snapshot
            .as_ref()
            .map_or_else(|| input.tools.clone(), |snapshot| snapshot.tools.clone());

        let fingerprints: Vec<[u8; 32]> = messages.iter().map(fingerprint).collect();
        let previous = {
            let state = self.inner.state.lock().unwrap();
            state.get(&input.session_id).cloned()
        };
        let mut unchanged_prefix_length = 0;
        if let Some(previous) = &previous {
            while unchanged_prefix_length < fingerprints.len()
                && previous.messages.get(unchanged_prefix_length)
                    == fingerprints.get(unchanged_prefix_length)
            {
                unchanged_prefix_length += 1;
            }
        }
        let new_checkpoint = previous.is_some()
            && checkpoint_boundary(messages.first())
            && checkpoint_summary(messages.get(1))
            && unchanged_prefix_length < 2;
        let refresh_time = !input.preview_only && new_checkpoint;
        let current_ms = (self.inner.now)();
        let current_date = iso_date(current_ms);
        let prompt_timestamp = previous
            .as_ref()
            .filter(|_| !refresh_time)
            .map_or(current_ms, |previous| previous.timestamp);
        let mut date_updates: Vec<DateUpdate> = if refresh_time {
            Vec::new()
        } else {
            previous.as_ref().map_or_else(Vec::new, |previous| {
                previous
                    .date_updates
                    .iter()
                    .filter(|update| update.index <= unchanged_prefix_length)
                    .cloned()
                    .collect()
            })
        };
        let last_date = date_updates
            .last()
            .map(|update| update.date.clone())
            .unwrap_or_else(|| iso_date(prompt_timestamp));
        if current_date != last_date {
            date_updates.push(DateUpdate {
                index: messages.len(),
                date: current_date.clone(),
                message: date_update_message(&current_date),
            });
        }
        let request_messages = apply_date_updates(&messages, &date_updates);

        let prompt = self.inner.assembler.assemble(
            &input.cwd,
            &input.provider,
            &input.model,
            &input.permission_mode,
            input.run_mode.as_deref(),
            &input.additional_working_directories,
            &effective_tools,
            input.custom_system_prompt.as_deref(),
            input.use_custom_system_prompt,
            input.append_system_prompt.as_deref(),
            self.inner.include_extensions_with_custom_system_prompt,
            input.surface == RuntimeContextSurface::SystemPrompt,
            prompt_timestamp,
        );
        let mut parts = if has_complete_contribution {
            contributed_sections
        } else {
            let mut parts = prompt.parts;
            parts.extend(contributed_sections);
            parts
        };

        if input.surface == RuntimeContextSurface::SystemPrompt {
            for context in &registered_runtime_contexts {
                parts.push(context.text.clone());
            }
        }
        let mut instruction_layers: Option<Vec<Layer>> = None;
        if let Some(discovery) = self.inner.instruction_discovery.clone() {
            let result = tokio::task::spawn_blocking(move || discovery.discover())
                .await
                .unwrap_or_else(|error| Err(error.to_string()));
            match result {
                Ok(layers) => {
                    if !layers.is_empty() {
                        let blocks = layers
                            .iter()
                            .map(|layer| {
                                format!(
                                    "Contents of {}{}:\n\n{}",
                                    layer.path,
                                    layer.scope.description(),
                                    layer.content
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        parts.push(format!(
                            "<project-instructions>\nProject instructions are shown below. Adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior.\n\n{blocks}\n</project-instructions>"
                        ));
                    }
                    instruction_layers = Some(layers);
                }
                Err(_) => diagnostics.push(Diagnostic {
                    code: "instruction_discovery_failed".into(),
                    severity: "warning",
                    message: "Failed to discover PILOTDECK.md instruction files.".into(),
                }),
            }
        }
        let joined = parts.join("\n\n");
        let mut runtime_contexts = vec![(
            "pilotdeck:user-context:0".to_owned(),
            prompt
                .sections
                .user_context
                .first()
                .cloned()
                .unwrap_or_default(),
        )];
        runtime_contexts.extend(
            registered_runtime_contexts
                .iter()
                .map(|context| (context.name.clone(), context.text.clone())),
        );
        let runtime_context_messages =
            build_runtime_context_messages(&runtime_contexts, input.surface);
        let projected_messages =
            insert_before_latest_user_request(request_messages, &runtime_context_messages);

        if !input.preview_only {
            self.inner.state.lock().unwrap().insert(
                input.session_id,
                TimeState {
                    timestamp: prompt_timestamp,
                    messages: fingerprints,
                    date_updates,
                },
            );
        }

        let mut materialization = JsObject::new();
        if let Some(step_id) = input.step_id {
            materialization.insert("stepId".into(), step_id);
        }
        if let Some(snapshot) = &contribution_snapshot {
            materialization.insert(
                "promptGeneration".into(),
                JsValue::Number(snapshot.generation as f64),
            );
        }
        materialization.insert(
            "runtimeContexts".into(),
            JsValue::Array(
                runtime_contexts
                    .iter()
                    .map(|(name, text)| {
                        object(vec![
                            ("name", JsValue::String(name.as_str().into())),
                            ("text", JsValue::String(text.as_str().into())),
                        ])
                    })
                    .collect(),
            ),
        );
        if !runtime_context_messages.is_empty() {
            materialization.insert(
                "runtimeContextMessages".into(),
                JsValue::Array(runtime_context_messages),
            );
        }
        if let Some(layers) = instruction_layers {
            materialization.insert(
                "instructionLayers".into(),
                JsValue::Array(
                    layers
                        .iter()
                        .map(|layer| {
                            object(vec![
                                ("scope", JsValue::String(layer.scope.as_str().into())),
                                ("path", JsValue::String(layer.path.as_str().into())),
                                ("content", JsValue::String(layer.content.as_str().into())),
                            ])
                        })
                        .collect(),
                ),
            );
        }
        let tool_count = effective_tools.len();
        Ok(object(vec![
            ("messages", JsValue::Array(projected_messages)),
            ("systemPrompt", JsValue::String(joined.into())),
            (
                "systemPromptParts",
                JsValue::Array(
                    parts
                        .into_iter()
                        .map(|part| JsValue::String(part.into()))
                        .collect(),
                ),
            ),
            ("tools", JsValue::Array(effective_tools)),
            (
                "diagnostics",
                JsValue::Array(
                    diagnostics
                        .into_iter()
                        .map(|diagnostic| {
                            object(vec![
                                ("code", JsValue::String(diagnostic.code.into())),
                                ("severity", JsValue::String(diagnostic.severity.into())),
                                ("message", JsValue::String(diagnostic.message.into())),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("boundaries", JsValue::Array(Vec::new())),
            (
                "metadata",
                object(vec![
                    ("droppedCount", JsValue::Number(dropped_count as f64)),
                    ("toolCount", JsValue::Number(tool_count as f64)),
                ]),
            ),
            ("materialization", JsValue::Object(materialization)),
        ]))
    }
}
impl ContextRuntime for DefaultContextRuntime {
    fn supports(&self, operation: Operation) -> bool {
        operation == Operation::PrepareForModel
    }
    fn prepare_for_model(&self, input: ContextInput) -> HostFuture<JsValue> {
        let runtime = self.clone();
        // `ContextInput` carries cancellation opaquely, like the tool runtime.
        // Adopt the host's one-shot `Cancellation`; an unrelated capability is
        // ignored, exactly like an `AbortSignal` that is never aborted.
        let abort = input
            .abort_signal
            .as_ref()
            .and_then(|signal| signal.downcast_ref::<Cancellation>())
            .cloned();
        Box::pin(async move { runtime.prepare(input.fields, abort).await })
    }
}

struct PrepareInput {
    session_id: Option<JsString>,
    session_id_text: String,
    turn_id: String,
    step_id: Option<JsValue>,
    cwd: String,
    surface: RuntimeContextSurface,
    provider: String,
    model: String,
    permission_mode: String,
    run_mode: Option<String>,
    additional_working_directories: Vec<String>,
    messages: Vec<JsValue>,
    tools: Vec<JsValue>,
    custom_system_prompt: Option<String>,
    use_custom_system_prompt: bool,
    append_system_prompt: Option<String>,
    max_messages: Option<(usize, String)>,
    preview_only: bool,
}
impl PrepareInput {
    fn parse(fields: &JsObject, default_surface: RuntimeContextSurface) -> Result<Self, HostError> {
        let value = |name: &str| fields.get(name).unwrap_or(&UNDEFINED);
        let messages = match value("messages") {
            JsValue::Array(messages) => messages
                .iter()
                .filter(|message| !is_runtime_context_message(message))
                .cloned()
                .collect(),
            _ => {
                return Err(
                    ModuleError::new("Context prepare input messages must be an array.").into(),
                );
            }
        };
        let tools = match value("tools") {
            JsValue::Array(tools) => tools.clone(),
            _ => {
                return Err(
                    ModuleError::new("Context prepare input tools must be an array.").into(),
                );
            }
        };
        let additional_working_directories = match value("additionalWorkingDirectories") {
            JsValue::Array(directories) => directories.iter().map(display).collect(),
            _ => Vec::new(),
        };
        let run_mode = match value("runMode") {
            value if truthy(value) => Some(display(value)),
            _ => None,
        };
        let custom = value("customSystemPrompt");
        let use_custom_system_prompt = !matches!(custom, JsValue::Undefined);
        let custom_system_prompt = match custom {
            JsValue::Undefined | JsValue::Null => None,
            JsValue::String(_) => Some(display(custom)),
            value if truthy(value) => {
                return Err(ModuleError::new(
                    "Context prepare input customSystemPrompt must be a string.",
                )
                .into());
            }
            _ => None,
        };
        let append_system_prompt = match value("appendSystemPrompt") {
            JsValue::String(_) if truthy(value("appendSystemPrompt")) => {
                Some(display(value("appendSystemPrompt")))
            }
            value if truthy(value) => {
                return Err(ModuleError::new(
                    "Context prepare input appendSystemPrompt must be a string.",
                )
                .into());
            }
            _ => None,
        };
        let max_messages = match value("maxMessages") {
            JsValue::Undefined => None,
            value => {
                let number = crate::js_value::to_number(value).map_err(|_| {
                    ModuleError::new("Context prepare input maxMessages is invalid.")
                })?;
                if number.is_finite() && number >= 0.0 {
                    Some((number.floor() as usize, display(value)))
                } else {
                    None
                }
            }
        };
        let surface = match value("runtimeContextSurface") {
            JsValue::Undefined | JsValue::Null => default_surface,
            value => RuntimeContextSurface::parse(value),
        };
        Ok(Self {
            session_id: fields
                .get("sessionId")
                .and_then(JsValue::as_string)
                .cloned(),
            step_id: fields
                .get("stepId")
                .filter(|value| !value.is_undefined())
                .cloned(),
            session_id_text: display(value("sessionId")),
            turn_id: display(value("turnId")),
            cwd: display(value("cwd")),
            surface,
            provider: display(value("provider")),
            model: display(value("model")),
            permission_mode: display(value("permissionMode")),
            run_mode,
            additional_working_directories,
            messages,
            tools,
            custom_system_prompt,
            use_custom_system_prompt,
            append_system_prompt,
            max_messages,
            preview_only: truthy(value("previewOnly")),
        })
    }
}

struct Diagnostic {
    code: String,
    severity: &'static str,
    message: String,
}
struct Warning {
    code: &'static str,
    message: String,
}
struct Projection {
    messages: Vec<JsValue>,
    dropped_count: usize,
    warnings: Vec<Warning>,
}
#[derive(Clone)]
struct DateUpdate {
    index: usize,
    date: String,
    message: JsValue,
}
#[derive(Clone)]
struct TimeState {
    timestamp: f64,
    messages: Vec<[u8; 32]>,
    date_updates: Vec<DateUpdate>,
}

fn project(messages: &[JsValue], max_messages: Option<(usize, &str)>) -> Projection {
    let mut warnings = Vec::new();
    let mut projected = messages.to_vec();
    let mut dropped_count = 0;
    if let Some((max, raw)) = max_messages
        && projected.len() > max
        && !has_compaction_checkpoint(&projected)
    {
        let (kept, dropped) = tool_pair_safe_truncate(projected, max);
        projected = kept;
        dropped_count = dropped;
        if dropped > 0 {
            warnings.push(Warning {
                code: "context_truncated",
                message: format!("Truncated {dropped} message(s) to respect maxMessages={raw}."),
            });
        }
    }
    projected = repair_tool_result_pairing(projected, &mut warnings);
    Projection {
        messages: projected,
        dropped_count,
        warnings,
    }
}

fn tool_pair_safe_truncate(messages: Vec<JsValue>, max: usize) -> (Vec<JsValue>, usize) {
    if messages.len() <= max {
        return (messages, 0);
    }
    let mut cut = messages.len() - max;
    while cut < messages.len() && is_tool_result_only(&messages[cut]) {
        cut += 1;
    }
    (messages[cut..].to_vec(), cut)
}

fn repair_tool_result_pairing(messages: Vec<JsValue>, warnings: &mut Vec<Warning>) -> Vec<JsValue> {
    let mut output = Vec::new();
    let mut pending: Vec<JsString> = Vec::new();
    let mut visible = HashSet::new();
    for message in &messages {
        if is_assistant(message) {
            for id in collect_tool_call_ids(message) {
                visible.insert(id);
            }
        }
    }
    for message in messages {
        if is_assistant(&message) {
            if !pending.is_empty() {
                inject_placeholder_results(&pending, &mut output, warnings);
                pending.clear();
            }
            pending = collect_tool_call_ids(&message);
            output.push(message);
            continue;
        }
        let pending_set: HashSet<JsString> = pending.iter().cloned().collect();
        let mut seen: Vec<JsString> = Vec::new();
        let mut media_refs: Vec<JsString> = Vec::new();
        for block in message_content(&message) {
            if is_direct_tool_result_block(block) {
                if let Some(id) = object_string(block, "toolCallId")
                    && !seen.contains(id)
                {
                    seen.push(id.clone());
                }
            } else if let Some(id) = media_reference_id(block)
                && !media_refs.contains(id)
            {
                media_refs.push(id.clone());
            }
        }
        let has_orphans = seen.iter().any(|id| !pending_set.contains(id))
            || media_refs.iter().any(|id| !visible.contains(id));
        let mut cleaned = message.clone();
        if has_orphans {
            let mut kept = Vec::new();
            for block in message_content(&message) {
                let direct_orphan = is_direct_tool_result_block(block)
                    && object_string(block, "toolCallId")
                        .is_some_and(|id| !pending_set.contains(id));
                let media_orphan =
                    media_reference_id(block).is_some_and(|id| !visible.contains(id));
                if direct_orphan || media_orphan {
                    warn_orphan(block, warnings);
                } else {
                    kept.push(block.clone());
                }
            }
            if kept.is_empty() {
                for id in &seen {
                    pending.retain(|pending_id| pending_id != id);
                }
                continue;
            }
            if let JsValue::Object(object) = &mut cleaned {
                object.insert("content".into(), JsValue::Array(kept));
            }
        }
        pending.retain(|id| !seen.contains(id));
        output.push(cleaned);
    }
    if !pending.is_empty() {
        inject_placeholder_results(&pending, &mut output, warnings);
    }
    output
}

fn inject_placeholder_results(
    ids: &[JsString],
    output: &mut Vec<JsValue>,
    warnings: &mut Vec<Warning>,
) {
    let blocks = ids
        .iter()
        .map(|id| {
            object(vec![
                ("type", JsValue::String("tool_result".into())),
                ("toolCallId", JsValue::String(id.clone())),
                (
                    "content",
                    JsValue::Array(vec![object(vec![
                        ("type", JsValue::String("text".into())),
                        ("text", JsValue::String("[result truncated]".into())),
                    ])]),
                ),
            ])
        })
        .collect();
    output.push(object(vec![
        ("role", JsValue::String("user".into())),
        ("content", JsValue::Array(blocks)),
    ]));
    for id in ids {
        warnings.push(Warning {
            code: "tool_result_injected",
            message: format!(
                "Injected placeholder tool_result for unmatched tool_call {}.",
                String::from_utf16_lossy(id.units())
            ),
        });
    }
}
fn warn_orphan(block: &JsValue, warnings: &mut Vec<Warning>) {
    let id = object_string(block, "toolCallId")
        .map(|id| String::from_utf16_lossy(id.units()))
        .unwrap_or_else(|| "undefined".into());
    warnings.push(Warning {
        code: "tool_result_orphaned",
        message: format!("tool_result {id} has no matching tool_call — removed."),
    });
}

fn has_compaction_checkpoint(messages: &[JsValue]) -> bool {
    messages.len() >= 2
        && message_has_text_prefix(&messages[0], "user", "<compact-boundary")
        && message_has_text_prefix(
            &messages[1],
            "assistant",
            "[CONTEXT COMPACTION - REFERENCE ONLY]",
        )
}
fn checkpoint_boundary(message: Option<&JsValue>) -> bool {
    message.is_some_and(|message| message_has_text_prefix(message, "user", "<compact-boundary"))
}
fn checkpoint_summary(message: Option<&JsValue>) -> bool {
    message.is_some_and(|message| {
        message_has_text_prefix(
            message,
            "assistant",
            "[CONTEXT COMPACTION - REFERENCE ONLY]",
        )
    })
}
fn message_has_text_prefix(message: &JsValue, role: &str, prefix: &str) -> bool {
    is_role(message, role)
        && message_content(message).iter().any(|block| {
            is_block_type(block, "text")
                && object_string(block, "text").is_some_and(|text| js_starts_with(text, prefix))
        })
}
fn js_starts_with(value: &JsString, prefix: &str) -> bool {
    let prefix: Vec<u16> = prefix.encode_utf16().collect();
    value.units().starts_with(&prefix)
}

fn apply_date_updates(messages: &[JsValue], updates: &[DateUpdate]) -> Vec<JsValue> {
    let mut output = Vec::new();
    let mut index = 0usize;
    for update in updates {
        let start = index.min(messages.len());
        let end = update.index.min(messages.len());
        if start < end {
            output.extend_from_slice(&messages[start..end]);
        }
        output.push(update.message.clone());
        index = update.index;
    }
    let start = index.min(messages.len());
    output.extend_from_slice(&messages[start..]);
    output
}
fn date_update_message(date: &str) -> JsValue {
    object(vec![
        ("role", JsValue::String("user".into())),
        (
            "content",
            JsValue::Array(vec![object(vec![
                ("type", JsValue::String("text".into())),
                (
                    "text",
                    JsValue::String(
                        format!(
                            "<date-update>\ncurrent_date: {date} (UTC)\nThe date has changed. Use this date for today and relative dates, superseding earlier environment dates.\n</date-update>"
                        )
                        .into(),
                    ),
                ),
            ])]),
        ),
        (
            "metadata",
            object(vec![
                ("synthetic", JsValue::Bool(true)),
                ("purpose", JsValue::String("date_update".into())),
            ]),
        ),
    ])
}

fn build_runtime_context_messages(
    contexts: &[(String, String)],
    surface: RuntimeContextSurface,
) -> Vec<JsValue> {
    if surface != RuntimeContextSurface::UserMessage || contexts.is_empty() {
        return Vec::new();
    }
    let content = contexts
        .iter()
        .map(|(name, text)| {
            object(vec![
                ("type", JsValue::String("text".into())),
                (
                    "text",
                    JsValue::String(
                        format!(
                            "<runtime-context name=\"{}\">\n{}\n</runtime-context>",
                            escape_xml_attribute(name),
                            text
                        )
                        .into(),
                    ),
                ),
            ])
        })
        .collect();
    vec![object(vec![
        ("role", JsValue::String("user".into())),
        ("content", JsValue::Array(content)),
        (
            "metadata",
            object(vec![
                ("synthetic", JsValue::Bool(true)),
                ("purpose", JsValue::String("runtime_context".into())),
            ]),
        ),
    ])]
}

fn insert_before_latest_user_request(
    messages: Vec<JsValue>,
    additions: &[JsValue],
) -> Vec<JsValue> {
    if additions.is_empty() {
        return messages;
    }
    let mut insertion = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        if is_real_user_request_message(message) {
            insertion = index;
            break;
        }
    }
    let mut output = Vec::with_capacity(messages.len() + additions.len());
    output.extend_from_slice(&messages[..insertion]);
    output.extend_from_slice(additions);
    output.extend_from_slice(&messages[insertion..]);
    output
}

const CONTINUATION_TEXT: &str =
    "[system: the conversation above has been compacted. please continue with the current task.]";
const INTERNAL_USER_TEXT_PREFIXES: [&str; 5] = [
    "<compact-boundary",
    "<snip-boundary",
    "<memory-context>",
    "<internal-compaction-control",
    "<hook_context",
];
fn is_real_user_request_message(message: &JsValue) -> bool {
    if !is_role(message, "user") {
        return false;
    }
    if message
        .as_object()
        .and_then(|message| message.get("metadata"))
        .and_then(JsValue::as_object)
        .and_then(|metadata| metadata.get("synthetic"))
        .is_some_and(|synthetic| matches!(synthetic, JsValue::Bool(true)))
    {
        return false;
    }
    message_content(message).iter().any(|block| {
        if is_direct_tool_result_block(block) || media_reference_id(block).is_some() {
            return false;
        }
        if !is_block_type(block, "text") {
            return true;
        }
        let Some(text) = object_string(block, "text") else {
            return false;
        };
        let text = String::from_utf16_lossy(text.units());
        let trimmed = js_trim(&text);
        !trimmed.is_empty()
            && trimmed != CONTINUATION_TEXT
            && !INTERNAL_USER_TEXT_PREFIXES
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
    })
}

fn is_runtime_context_message(message: &JsValue) -> bool {
    message
        .as_object()
        .and_then(|message| message.get("metadata"))
        .and_then(JsValue::as_object)
        .and_then(|metadata| metadata.get("purpose"))
        .and_then(JsValue::as_string)
        .is_some_and(|purpose| purpose.equals_str("runtime_context"))
}
fn collect_tool_call_ids(message: &JsValue) -> Vec<JsString> {
    message_content(message)
        .iter()
        .filter(|block| is_block_type(block, "tool_call"))
        .filter_map(|block| object_string(block, "id").cloned())
        .collect()
}
fn is_tool_result_only(message: &JsValue) -> bool {
    let content = message_content(message);
    !content.is_empty()
        && content
            .iter()
            .all(|block| is_direct_tool_result_block(block) || media_reference_id(block).is_some())
}
fn is_direct_tool_result_block(block: &JsValue) -> bool {
    is_block_type(block, "tool_result") || is_block_type(block, "tool_result_reference")
}
fn media_reference_id(block: &JsValue) -> Option<&JsString> {
    is_block_type(block, "media_reference")
        .then(|| object_string(block, "toolCallId"))
        .flatten()
        .filter(|id| !id.is_empty())
}
fn is_assistant(message: &JsValue) -> bool {
    is_role(message, "assistant")
}
fn is_role(message: &JsValue, role: &str) -> bool {
    message
        .as_object()
        .and_then(|message| message.get("role"))
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str(role))
}
fn is_block_type(block: &JsValue, kind: &str) -> bool {
    block
        .as_object()
        .and_then(|block| block.get("type"))
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str(kind))
}
fn object_string<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsString> {
    value
        .as_object()
        .and_then(|value| value.get(key))
        .and_then(JsValue::as_string)
}
fn message_content(message: &JsValue) -> &[JsValue] {
    static EMPTY: [JsValue; 0] = [];
    message
        .as_object()
        .and_then(|message| message.get("content"))
        .and_then(|content| match content {
            JsValue::Array(content) => Some(content.as_slice()),
            _ => None,
        })
        .unwrap_or(&EMPTY)
}

fn fingerprint(message: &JsValue) -> [u8; 32] {
    let mut stable = JsObject::new();
    stable.insert(
        "role".into(),
        message
            .as_object()
            .and_then(|message| message.get("role"))
            .cloned()
            .unwrap_or(JsValue::Undefined),
    );
    stable.insert(
        "content".into(),
        message
            .as_object()
            .and_then(|message| message.get("content"))
            .cloned()
            .unwrap_or(JsValue::Undefined),
    );
    let mut hasher = Sha256::new();
    hasher.update(stable_serialize(&JsValue::Object(stable)).as_bytes());
    hasher.finalize().into()
}

/// `context/cache/CachePlan.ts:stableSerialize`, including JS's `undefined`
/// interpolation and UTF-16 key ordering.
fn stable_serialize(value: &JsValue) -> String {
    match value {
        JsValue::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|item| {
                    if item.is_undefined() {
                        String::new()
                    } else {
                        stable_serialize(item)
                    }
                })
                .collect();
            format!("[{}]", parts.join(","))
        }
        JsValue::Object(object) => {
            let mut entries: Vec<(&JsString, &JsValue)> = object
                .entries()
                .iter()
                .map(|(key, value)| (key, value))
                .collect();
            entries.sort_by(|a, b| a.0.units().cmp(b.0.units()));
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        stringify_scalar(&JsValue::String(key.clone())),
                        stable_serialize(value)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        JsValue::Undefined => "undefined".into(),
        value => stringify_scalar(value),
    }
}
fn stringify_scalar(value: &JsValue) -> String {
    pilotdeck_protocol::stringify_json(value)
        .ok()
        .flatten()
        .unwrap_or_else(|| "null".into())
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
fn contribution_value(value: Option<&JsValue>) -> Option<String> {
    match value {
        None | Some(JsValue::Undefined) => None,
        Some(value) => Some(display(value)),
    }
}
fn display(value: &JsValue) -> String {
    crate::js_value::display(value).unwrap_or_default()
}
fn object(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}
fn system_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_millis() as f64)
}
