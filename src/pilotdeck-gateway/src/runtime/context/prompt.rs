//! System-prompt assembly, ported from `context/prompt/PromptAssembler.ts`.
//!
//! The assembler only depends on a read-only extension projection plus the
//! caller's resolved prompt timestamp. Store-backed instruction discovery is a
//! separate concern in `instructions`.
use crate::metadata::js_trim;
use crate::project_files::{node_collator, node_locale};
use icu_locale_core::Locale;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributedCommand {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributedSkill {
    pub name: String,
    pub description: Option<String>,
    /// Absolute path to the SKILL.md selected by the extension generation.
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerInstruction {
    pub server_name: String,
    pub instructions: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributedPrompt {
    pub name: String,
    pub content: String,
}

/// Context-side projection of plugin-derived prompt material.
///
/// `list_prompt_contributions` is the extension-generation adapter input; the
/// session-owned contribution registry itself lives in this module.
pub trait ExtensionResolver: Send + Sync {
    fn list_commands(&self) -> Vec<ContributedCommand> {
        Vec::new()
    }
    fn list_skills(&self) -> Vec<ContributedSkill> {
        Vec::new()
    }
    fn list_mcp_instructions(&self) -> Vec<McpServerInstruction> {
        Vec::new()
    }
    fn list_prompt_contributions(&self) -> Vec<ContributedPrompt> {
        Vec::new()
    }
}

#[derive(Clone, Copy, Default)]
pub struct NullExtensionResolver;
impl ExtensionResolver for NullExtensionResolver {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptSections {
    pub default_system_prompt: Vec<String>,
    pub user_context: Vec<String>,
    pub system_context: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptResult {
    pub parts: Vec<String>,
    pub joined: String,
    pub sections: PromptSections,
}
pub struct PromptAssembler {
    extension: std::sync::Arc<dyn ExtensionResolver>,
    locale: Locale,
    platform: String,
    node_version: String,
}
impl PromptAssembler {
    pub fn new(extension: std::sync::Arc<dyn ExtensionResolver>) -> Self {
        Self {
            extension,
            locale: node_locale(|key| std::env::var(key).ok()),
            platform: process_platform().into(),
            node_version: node_version().into(),
        }
    }
    /// Test/wiring seam: the host has no Node identity of its own.
    pub fn with_environment(
        extension: std::sync::Arc<dyn ExtensionResolver>,
        locale: Locale,
        platform: impl Into<String>,
        node_version: impl Into<String>,
    ) -> Self {
        Self {
            extension,
            locale,
            platform: platform.into(),
            node_version: node_version.into(),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        &self,
        cwd: &str,
        provider: &str,
        model: &str,
        permission_mode: &str,
        run_mode: Option<&str>,
        additional_working_directories: &[String],
        tools: &[pilotdeck_protocol::JsValue],
        custom_system_prompt: Option<&str>,
        use_custom_system_prompt: bool,
        append_system_prompt: Option<&str>,
        include_extensions_with_custom_system_prompt: bool,
        include_user_context_in_system_prompt: bool,
        timestamp_ms: f64,
    ) -> PromptResult {
        let sections = self.build_sections(
            cwd,
            provider,
            model,
            permission_mode,
            run_mode,
            additional_working_directories,
            tools,
            timestamp_ms,
        );
        let mut parts = Vec::new();
        let use_custom = use_custom_system_prompt;
        if use_custom {
            if let Some(custom) = custom_system_prompt {
                let trimmed = js_trim(custom);
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_owned());
                }
            }
        } else {
            parts.extend(sections.default_system_prompt.iter().cloned());
        }
        if include_user_context_in_system_prompt {
            parts.extend(sections.user_context.iter().cloned());
        }
        if !use_custom || include_extensions_with_custom_system_prompt {
            parts.extend(sections.system_context.iter().cloned());
        }
        if let Some(append) = append_system_prompt {
            let trimmed = js_trim(append);
            if !trimmed.is_empty() {
                parts.push(trimmed.to_owned());
            }
        }
        let joined = parts.join("\n\n");
        PromptResult {
            parts,
            joined,
            sections,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_sections(
        &self,
        cwd: &str,
        provider: &str,
        model: &str,
        permission_mode: &str,
        run_mode: Option<&str>,
        additional_working_directories: &[String],
        tools: &[pilotdeck_protocol::JsValue],
        timestamp_ms: f64,
    ) -> PromptSections {
        PromptSections {
            default_system_prompt: self.build_default_system_prompt(
                permission_mode,
                run_mode,
                additional_working_directories,
                tools,
            ),
            user_context: build_user_context(
                cwd,
                provider,
                model,
                permission_mode,
                run_mode,
                &self.platform,
                &self.node_version,
            ),
            system_context: self.build_system_context(timestamp_ms),
        }
    }

    fn build_default_system_prompt(
        &self,
        permission_mode: &str,
        run_mode: Option<&str>,
        additional_working_directories: &[String],
        tools: &[pilotdeck_protocol::JsValue],
    ) -> Vec<String> {
        let has_web_search = has_tool(tools, "web_search");
        let has_web_fetch = has_tool(tools, "web_fetch");
        let documentation_lookup_policy = if has_web_search {
            "When implementing code against an API, SDK, framework, CLI, config schema, or file format whose usage is not clear from local source, installed types, examples, or project docs, search for official documentation before writing or changing code. Prefer versioned official docs and existing in-repo call sites. Use web_search for discovery and web_fetch for the relevant docs page. Do not guess unfamiliar signatures, options, or output shapes when a quick lookup can resolve them. If network tools are unavailable or denied, state the uncertainty and proceed conservatively."
        } else if has_web_fetch {
            "When implementing code against an API, SDK, framework, CLI, config schema, or file format whose usage is not clear, prefer installed types, local source, examples, and project docs. If a relevant official documentation URL is already known, use web_fetch to inspect it. Otherwise state the uncertainty and proceed conservatively."
        } else {
            "When implementing code against an API, SDK, framework, CLI, config schema, or file format whose usage is not clear, prefer installed types, local source, examples, and project docs. State any remaining uncertainty and proceed conservatively."
        };
        let mut lines: Vec<String> = vec![
            "You are PilotDeck, an AI agent runtime. You execute tasks across CLI, TUI, web, and chat channels by calling structured tools and reasoning over their results.".into(),
            "Operate decisively: prefer using available tools to gather facts before answering, prefer concise replies, and surface uncertainty when present.".into(),
            String::new(),
            "Documentation lookup policy:".into(),
            documentation_lookup_policy.into(),
            String::new(),
            "Parallel delegation policy:".into(),
            "When the agent tool is available and the task contains two or more independent, repetitive, or separable workstreams, prefer launching multiple subagents in the same assistant message instead of waiting for one to finish before starting the next. Good parallel candidates include inspecting multiple files/modules, researching independent APIs, checking several artifacts, or comparing alternatives. Do not parallelize tasks that write to the same files, depend on another subagent's output, require shared ordering, or need user approval between steps. Give each subagent a self-contained prompt, distinct scope, and expected output. After all sibling results return, synthesize them yourself and decide the next step.".into(),
            String::new(),
            "Reusable script workflow:".into(),
            "When code is more than a tiny one-off command, or you expect to rerun it with changed parameters, write it to a workspace file first with write_file, then run it with bash. Prefer scripts with CLI arguments, environment variables, or a small config section so parameters can be adjusted with edit_file or command args. Do not pack large Python/JS/shell programs into bash heredocs or long `python -c` / `node -e` strings. After each run, inspect output and edit the saved script instead of regenerating a new inline command.".into(),
        ];
        let permission_line = format_permission_mode(permission_mode);
        if !permission_line.is_empty() {
            lines.push(String::new());
            lines.push(permission_line);
        }
        if let Some(run_mode_line) = format_run_mode(run_mode) {
            lines.push(run_mode_line);
        }
        if !additional_working_directories.is_empty() {
            lines.push(String::new());
            lines.push("Additional working directories you may operate in:".into());
            for dir in additional_working_directories {
                lines.push(format!("- {dir}"));
            }
        }
        let mcp_block = self.format_mcp_instructions();
        if !mcp_block.is_empty() {
            lines.push(String::new());
            lines.push("Connected MCP server instructions:".into());
            lines.push(mcp_block);
        }
        vec![lines.join("\n")]
    }

    fn build_system_context(&self, timestamp_ms: f64) -> Vec<String> {
        let mut sections = Vec::new();
        let day = iso_date(timestamp_ms);
        sections.push(format!("<environment>\nnow: {day}\n</environment>"));
        let commands = self.extension.list_commands();
        if !commands.is_empty() {
            sections.push(format_commands(&commands));
        }
        let skills = self.extension.list_skills();
        if !skills.is_empty() {
            sections.push(format_skills(&skills));
        }
        sections
    }

    fn format_mcp_instructions(&self) -> String {
        let mut populated: Vec<(String, String)> = self
            .extension
            .list_mcp_instructions()
            .into_iter()
            .filter_map(|entry| {
                let instructions = entry.instructions?;
                let trimmed = js_trim(&instructions);
                if trimmed.is_empty() {
                    return None;
                }
                Some((entry.server_name, trimmed.to_owned()))
            })
            .collect();
        if populated.is_empty() {
            return String::new();
        }
        // Prompt-cache stability: Node's default localeCompare order.
        if let Ok(collator) = node_collator(&self.locale) {
            populated.sort_by(|a, b| collator.compare(&a.0, &b.0));
        } else {
            populated.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        }
        let mut lines = vec!["<mcp-instructions>".to_owned()];
        for (server_name, instructions) in populated {
            lines.push(format!(
                "<server name=\"{}\">",
                escape_xml_attribute(&server_name)
            ));
            lines.push(instructions);
            lines.push("</server>".into());
        }
        lines.push("</mcp-instructions>".into());
        lines.join("\n")
    }
}

fn has_tool(tools: &[pilotdeck_protocol::JsValue], name: &str) -> bool {
    tools.iter().any(|tool| {
        tool.as_object()
            .and_then(|tool| tool.get("name"))
            .and_then(pilotdeck_protocol::JsValue::as_string)
            .is_some_and(|value| value.equals_str(name))
    })
}
fn build_user_context(
    cwd: &str,
    provider: &str,
    model: &str,
    permission_mode: &str,
    run_mode: Option<&str>,
    platform: &str,
    node_version: &str,
) -> Vec<String> {
    let mut lines = vec![
        "<user-context>".to_owned(),
        format!("cwd: {cwd}"),
        "IMPORTANT: When the user does not specify an explicit file path, all file paths in tool calls MUST be relative to the cwd above — use \"foo.html\", not an absolute path like \"/home/user/foo.html\". If the user explicitly provides a path, respect their choice.".to_owned(),
        format!("model: {provider}/{model}"),
        format!("permission_mode: {permission_mode}"),
    ];
    if let Some(run_mode) = run_mode.filter(|mode| !mode.is_empty()) {
        lines.push(format!("run_mode: {run_mode}"));
    }
    lines.push(format!("platform: {platform}"));
    lines.push(format!("node: {node_version}"));
    lines.push("</user-context>".into());
    vec![lines.join("\n")]
}
fn process_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        "solaris" => "sunos",
        other => other,
    }
}
fn node_version() -> &'static str {
    // The Rust host has no Node runtime. Integration must supply this through
    // Options; the pinned baseline version keeps the default deterministic.
    "v22.22.0"
}
fn format_permission_mode(mode: &str) -> String {
    match mode {
        "default" => {
            "Permission mode: default — write/shell tools require explicit approval.".into()
        }
        "plan" => "Permission mode: plan — read-only planning mode; implementation changes are blocked at tool runtime.".into(),
        "bypassPermissions" => {
            "Permission mode: bypassPermissions — all tools are auto-approved; act conservatively."
                .into()
        }
        other => format!("Permission mode: {other}"),
    }
}
fn format_run_mode(mode: Option<&str>) -> Option<String> {
    match mode {
        Some("ask") => Some("Run mode: ask — read-only analysis mode; write/action tools are blocked at tool runtime even when permission mode is bypassPermissions.".into()),
        Some("plan") => Some("Run mode: plan — planning mode is active.".into()),
        _ => None,
    }
}
fn format_commands(commands: &[ContributedCommand]) -> String {
    let mut lines = vec!["<available-commands>".to_owned()];
    for command in commands {
        let hint = command
            .argument_hint
            .as_deref()
            .filter(|hint| !hint.is_empty())
            .map_or_else(String::new, |hint| format!(" {hint}"));
        let description = command
            .description
            .as_deref()
            .filter(|description| !description.is_empty())
            .map_or_else(String::new, |description| format!(" — {description}"));
        lines.push(format!("- /{}{hint}{description}", command.name));
    }
    lines.push("</available-commands>".into());
    lines.join("\n")
}
fn format_skills(skills: &[ContributedSkill]) -> String {
    let mut lines = vec![
        "<available-skills>".to_owned(),
        "Use the read_skill tool to load the full content of any skill listed below. Each entry includes the exact SKILL.md selected by the runtime.".to_owned(),
        "Resolve relative references, scripts, and assets against the directory containing that SKILL.md.".to_owned(),
        "Do not search the user's home directory to rediscover a skill or infer runtime/cache paths; use the listed file and paths or commands returned by the skill.".to_owned(),
    ];
    for skill in skills {
        let description = skill
            .description
            .as_deref()
            .filter(|description| !description.is_empty())
            .map_or_else(String::new, |description| format!(" — {description}"));
        lines.push(format!(
            "- {}{description} (file: {})",
            skill.name, skill.path
        ));
    }
    lines.push("</available-skills>".into());
    lines.join("\n")
}

pub(crate) fn escape_xml_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// UTC `YYYY-MM-DD` slice of `new Date(ms).toISOString()`.
pub(crate) fn iso_date(timestamp_ms: f64) -> String {
    let ms = timestamp_ms as i64;
    let days = ms.div_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}
/// `Date.prototype.toISOString()` for the current wall clock, used where the
/// TS baseline stamps a `Date` that must cross the JSON boundary.
pub(crate) fn iso_timestamp_now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or(0.0);
    iso_timestamp(ms)
}

/// UTC `YYYY-MM-DDTHH:MM:SS.mmmZ` for years 0000–9999 and finite values in
/// the JS Date range `|ms| <= 8.64e15`. Inside that domain this matches
/// `new Date(ms).toISOString()`; extended `±YYYYYY` years, NaN, and values
/// outside the range are unsupported. The only caller is `iso_timestamp_now`
/// (current wall clock), so those boundaries are unreachable here.
pub(crate) fn iso_timestamp(timestamp_ms: f64) -> String {
    let ms = timestamp_ms as i64;
    let days = ms.div_euclid(86_400_000);
    let day_ms = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        day_ms / 3_600_000,
        day_ms / 60_000 % 60,
        day_ms / 1_000 % 60,
        day_ms % 1_000
    )
}

/// Howard Hinnant's civil-from-days, with day 0 = 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

// ---------------------------------------------------------------- contributions

pub type ProviderFuture<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send>>;

#[derive(Clone)]
pub struct ContributionContext {
    pub session_id: String,
    pub turn_id: String,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub permission_mode: String,
    pub run_mode: Option<String>,
    pub additional_working_directories: Vec<String>,
    /// TS `abortSignal`; `prepare_for_model` adopts the request's host
    /// [`crate::host::Cancellation`]. Providers see the same one-shot signal.
    pub abort: Option<crate::host::Cancellation>,
}
impl ContributionContext {
    /// TS `context.abortSignal?.throwIfAborted()`.
    ///
    /// The default TS reason is a DOMException whose message is exactly
    /// `This operation was aborted`; `Cancellation` stores the synthetic
    /// `host_abort` marker for an absent reason, so map that back to the
    /// default before reporting. An explicit string reason is thrown verbatim,
    /// like `controller.abort("reason")`.
    pub(crate) fn throw_if_aborted(&self) -> Result<(), String> {
        let Some(signal) = &self.abort else {
            return Ok(());
        };
        if !signal.is_cancelled() {
            return Ok(());
        }
        Err(
            match signal
                .reason()
                .filter(|reason| !reason.equals_str("host_abort"))
            {
                Some(reason) => String::from_utf16_lossy(reason.units()),
                None => "This operation was aborted".into(),
            },
        )
    }
}
/// A dynamic text provider returns a JS value, not a `String`: TS validates
/// `typeof value !== "string"` *after* the provider resolves, so the value can
/// be non-string at runtime even though the TS type says `string`.
pub type DynamicTextProvider =
    Arc<dyn Fn(&ContributionContext) -> ProviderFuture<JsValue> + Send + Sync>;
#[derive(Clone)]
pub enum TextProvider {
    Static(String),
    Dynamic(DynamicTextProvider),
}
#[derive(Clone)]
pub struct SectionContribution {
    pub name: String,
    pub order: f64,
    pub text: TextProvider,
    pub complete: bool,
}
#[derive(Clone)]
pub struct RuntimeContextContribution {
    pub name: String,
    pub order: f64,
    pub text: TextProvider,
}
pub type VariableProvider =
    Arc<dyn Fn(&ContributionContext) -> ProviderFuture<Option<String>> + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub struct AssembledSection {
    pub name: String,
    pub order: f64,
    pub text: String,
    pub complete: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AssembledRuntimeContext {
    pub name: String,
    pub order: f64,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct PromptContributionSnapshot {
    pub generation: u64,
    pub sections: Vec<AssembledSection>,
    pub runtime_contexts: Vec<AssembledRuntimeContext>,
    /// Own-property insertion order; `None` models JS `undefined`.
    pub variables: Vec<(String, Option<String>)>,
    /// `captureToolSchemas`: name / optional description / cloned inputSchema.
    pub tools: Vec<JsValue>,
}
#[derive(Default)]
pub struct SnapshotOptions<'a> {
    pub tool_schemas: Option<&'a [JsValue]>,
    pub variables: Vec<(String, Option<String>)>,
}

struct Registered<T> {
    value: T,
    token: u64,
}
struct RegistryState {
    generation: u64,
    sections: HashMap<String, Registered<SectionContribution>>,
    runtime_contexts: HashMap<String, Registered<RuntimeContextContribution>>,
    variables: HashMap<String, Registered<VariableProvider>>,
    disposed: bool,
}
struct RegistryInner {
    name: String,
    state: Mutex<RegistryState>,
}
#[derive(Clone)]
pub struct PromptContributionRegistry {
    inner: Arc<RegistryInner>,
}
#[derive(Clone, Copy)]
enum ContributionKind {
    Section,
    RuntimeContext,
    Variable,
}
pub struct ContributionRegistration {
    inner: Arc<RegistryInner>,
    kind: ContributionKind,
    name: String,
    token: u64,
}
impl ContributionRegistration {
    pub fn active(&self) -> bool {
        let state = self.inner.state.lock().unwrap();
        if state.disposed {
            return false;
        }
        match self.kind {
            ContributionKind::Section => state
                .sections
                .get(&self.name)
                .is_some_and(|entry| entry.token == self.token),
            ContributionKind::RuntimeContext => state
                .runtime_contexts
                .get(&self.name)
                .is_some_and(|entry| entry.token == self.token),
            ContributionKind::Variable => state
                .variables
                .get(&self.name)
                .is_some_and(|entry| entry.token == self.token),
        }
    }
    pub fn dispose(&self) {
        let mut state = self.inner.state.lock().unwrap();
        let current = match self.kind {
            ContributionKind::Section => state
                .sections
                .get(&self.name)
                .is_some_and(|entry| entry.token == self.token),
            ContributionKind::RuntimeContext => state
                .runtime_contexts
                .get(&self.name)
                .is_some_and(|entry| entry.token == self.token),
            ContributionKind::Variable => state
                .variables
                .get(&self.name)
                .is_some_and(|entry| entry.token == self.token),
        };
        if !current {
            return;
        }
        match self.kind {
            ContributionKind::Section => {
                state.sections.remove(&self.name);
            }
            ContributionKind::RuntimeContext => {
                state.runtime_contexts.remove(&self.name);
            }
            ContributionKind::Variable => {
                state.variables.remove(&self.name);
            }
        }
        state.generation += 1;
    }
}

impl PromptContributionRegistry {
    pub fn new(name: Option<&str>) -> Self {
        let name = name
            .map(js_trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("prompt-contributions")
            .to_owned();
        Self {
            inner: Arc::new(RegistryInner {
                name,
                state: Mutex::new(RegistryState {
                    generation: 0,
                    sections: HashMap::new(),
                    runtime_contexts: HashMap::new(),
                    variables: HashMap::new(),
                    disposed: false,
                }),
            }),
        }
    }
    pub fn name(&self) -> &str {
        &self.inner.name
    }
    pub fn generation(&self) -> u64 {
        self.inner.state.lock().unwrap().generation
    }
    pub fn state(&self) -> &'static str {
        if self.inner.state.lock().unwrap().disposed {
            "disposed"
        } else {
            "active"
        }
    }
    pub fn register_section(
        &self,
        contribution: SectionContribution,
    ) -> Result<ContributionRegistration, String> {
        let name = validate_contribution_name(&contribution.name, "prompt section")?;
        validate_order(contribution.order, &format!("prompt section \"{name}\""))?;
        let entry = Registered {
            value: SectionContribution {
                name: name.clone(),
                complete: contribution.complete,
                ..contribution
            },
            token: 0,
        };
        self.insert(ContributionKind::Section, name, entry)
    }
    pub fn register_runtime_context(
        &self,
        contribution: RuntimeContextContribution,
    ) -> Result<ContributionRegistration, String> {
        let name = validate_contribution_name(&contribution.name, "prompt runtime context")?;
        validate_order(
            contribution.order,
            &format!("prompt runtime context \"{name}\""),
        )?;
        let entry = Registered {
            value: RuntimeContextContribution {
                name: name.clone(),
                ..contribution
            },
            token: 0,
        };
        self.insert_runtime_context(name, entry)
    }
    pub fn register_variable(
        &self,
        name: &str,
        provider: VariableProvider,
    ) -> Result<ContributionRegistration, String> {
        let name = validate_contribution_name(name, "prompt variable")?;
        if !valid_variable_name(&name) {
            return Err(format!(
                "invalid prompt variable name \"{name}\" (must match /^[a-z][a-z0-9_]*$/)"
            ));
        }
        self.insert_variable(
            name,
            Registered {
                value: provider,
                token: 0,
            },
        )
    }

    fn insert(
        &self,
        kind: ContributionKind,
        name: String,
        mut entry: Registered<SectionContribution>,
    ) -> Result<ContributionRegistration, String> {
        let mut state = self.inner.state.lock().unwrap();
        self.assert_active(&state, &format!("register prompt section {name}"))?;
        if state.sections.contains_key(&name) {
            return Err(format!(
                "prompt section \"{name}\" is already registered in {}",
                self.inner.name
            ));
        }
        state.generation += 1;
        entry.token = state.generation;
        let token = entry.token;
        state.sections.insert(name.clone(), entry);
        Ok(ContributionRegistration {
            inner: self.inner.clone(),
            kind,
            name,
            token,
        })
    }
    fn insert_runtime_context(
        &self,
        name: String,
        mut entry: Registered<RuntimeContextContribution>,
    ) -> Result<ContributionRegistration, String> {
        let mut state = self.inner.state.lock().unwrap();
        self.assert_active(&state, &format!("register prompt runtime context {name}"))?;
        if state.runtime_contexts.contains_key(&name) {
            return Err(format!(
                "prompt runtime context \"{name}\" is already registered in {}",
                self.inner.name
            ));
        }
        state.generation += 1;
        entry.token = state.generation;
        let token = entry.token;
        state.runtime_contexts.insert(name.clone(), entry);
        Ok(ContributionRegistration {
            inner: self.inner.clone(),
            kind: ContributionKind::RuntimeContext,
            name,
            token,
        })
    }
    fn insert_variable(
        &self,
        name: String,
        mut entry: Registered<VariableProvider>,
    ) -> Result<ContributionRegistration, String> {
        let mut state = self.inner.state.lock().unwrap();
        self.assert_active(&state, &format!("register prompt variable {name}"))?;
        if state.variables.contains_key(&name) {
            return Err(format!(
                "prompt variable \"{name}\" is already registered in {}",
                self.inner.name
            ));
        }
        state.generation += 1;
        entry.token = state.generation;
        let token = entry.token;
        state.variables.insert(name.clone(), entry);
        Ok(ContributionRegistration {
            inner: self.inner.clone(),
            kind: ContributionKind::Variable,
            name,
            token,
        })
    }
    fn assert_active(&self, state: &RegistryState, action: &str) -> Result<(), String> {
        if state.disposed {
            return Err(format!(
                "Cannot {action}; prompt contribution registry {} is disposed.",
                self.inner.name
            ));
        }
        Ok(())
    }

    pub async fn snapshot(
        &self,
        context: &ContributionContext,
        options: SnapshotOptions<'_>,
    ) -> Result<PromptContributionSnapshot, String> {
        let (generation, mut sections, mut runtime_contexts, mut variables) = {
            let state = self.inner.state.lock().unwrap();
            if state.disposed {
                return Err(format!(
                    "Cannot capture a prompt contribution snapshot; prompt contribution registry {} is disposed.",
                    self.inner.name
                ));
            }
            context.throw_if_aborted()?;
            (
                state.generation,
                state
                    .sections
                    .values()
                    .map(|entry| entry.value.clone())
                    .collect::<Vec<_>>(),
                state
                    .runtime_contexts
                    .values()
                    .map(|entry| entry.value.clone())
                    .collect::<Vec<_>>(),
                state
                    .variables
                    .iter()
                    .map(|(name, entry)| (name.clone(), entry.value.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        let tools = capture_tool_schemas(options.tool_schemas)?;
        sections.sort_by(compare_sections);
        runtime_contexts.sort_by(compare_runtime_contexts);
        variables.sort_by(|left, right| compare_names(&left.0, &right.0));
        let complete_names: Vec<String> = sections
            .iter()
            .filter(|section| section.complete)
            .map(|section| section.name.clone())
            .collect();
        if complete_names.len() > 1 {
            return Err(format!(
                "Multiple complete prompt sections are visible: {}",
                complete_names.join(", ")
            ));
        }
        let mut assembled_sections = Vec::with_capacity(sections.len());
        for section in sections {
            let text = resolve_text(
                &section.text,
                context,
                &format!("prompt section \"{}\"", section.name),
            )
            .await?;
            assembled_sections.push(AssembledSection {
                name: section.name,
                order: section.order,
                text,
                complete: section.complete,
            });
        }
        let mut assembled_runtime_contexts = Vec::with_capacity(runtime_contexts.len());
        for contribution in runtime_contexts {
            let text = resolve_text(
                &contribution.text,
                context,
                &format!("prompt runtime context \"{}\"", contribution.name),
            )
            .await?;
            assembled_runtime_contexts.push(AssembledRuntimeContext {
                name: contribution.name,
                order: contribution.order,
                text,
            });
        }
        let mut assembled_variables = options.variables;
        for (name, provider) in variables {
            context.throw_if_aborted()?;
            let value = provider(context).await?;
            context.throw_if_aborted()?;
            if let Some(entry) = assembled_variables
                .iter_mut()
                .find(|(existing, _)| existing == &name)
            {
                entry.1 = value;
            } else {
                assembled_variables.push((name, value));
            }
        }
        Ok(PromptContributionSnapshot {
            generation,
            sections: assembled_sections,
            runtime_contexts: assembled_runtime_contexts,
            variables: assembled_variables,
            tools,
        })
    }

    pub fn dispose(&self) {
        let mut state = self.inner.state.lock().unwrap();
        if state.disposed {
            return;
        }
        state.sections.clear();
        state.runtime_contexts.clear();
        state.variables.clear();
        state.disposed = true;
        state.generation += 1;
    }
}

impl PromptAssembler {
    /// `registerExtensionPromptContributions`: plugin sections follow native
    /// identity/instruction sections and sort by `localeCompare(name)`.
    pub fn register_extension_prompt_contributions(
        &self,
        registry: &PromptContributionRegistry,
    ) -> Result<Vec<ContributionRegistration>, String> {
        let mut contributions = self.extension.list_prompt_contributions();
        if let Ok(collator) = node_collator(&self.locale) {
            contributions.sort_by(|left, right| collator.compare(&left.name, &right.name));
        } else {
            contributions
                .sort_by(|left, right| left.name.encode_utf16().cmp(right.name.encode_utf16()));
        }
        contributions
            .into_iter()
            .map(|contribution| {
                registry.register_section(SectionContribution {
                    name: contribution.name,
                    order: 10_000.0,
                    text: TextProvider::Static(contribution.content),
                    complete: false,
                })
            })
            .collect()
    }
}

pub fn render_prompt_contribution_sections(
    snapshot: &PromptContributionSnapshot,
) -> Result<Vec<String>, String> {
    let complete = snapshot.sections.iter().find(|section| section.complete);
    let sections: Vec<&AssembledSection> = complete
        .map(|section| vec![section])
        .unwrap_or_else(|| snapshot.sections.iter().collect());
    let mut output = Vec::new();
    for section in sections {
        let text = interpolate_prompt_variables(
            &section.text,
            &snapshot.variables,
            &format!("prompt section \"{}\"", section.name),
        )?;
        if !text.is_empty() {
            output.push(text);
        }
    }
    Ok(output)
}
pub fn render_prompt_runtime_context_sections(
    snapshot: &PromptContributionSnapshot,
) -> Result<Vec<AssembledRuntimeContext>, String> {
    let mut output = Vec::new();
    for context in &snapshot.runtime_contexts {
        let text = interpolate_prompt_variables(
            &context.text,
            &snapshot.variables,
            &format!("prompt runtime context \"{}\"", context.name),
        )?;
        if !text.is_empty() {
            output.push(AssembledRuntimeContext {
                name: context.name.clone(),
                order: context.order,
                text,
            });
        }
    }
    Ok(output)
}

async fn resolve_text(
    provider: &TextProvider,
    context: &ContributionContext,
    label: &str,
) -> Result<String, String> {
    context.throw_if_aborted()?;
    let value = match provider {
        TextProvider::Static(text) => JsValue::String(text.clone().into()),
        // A rejected provider propagates its own error unchanged, exactly like
        // TS `await provider(context)`. Only a resolved non-string hits the
        // TypeError-shaped message below.
        TextProvider::Dynamic(provider) => provider(context).await?,
    };
    context.throw_if_aborted()?;
    match &value {
        JsValue::String(text) => Ok(String::from_utf16_lossy(text.units())),
        _ => Err(format!("{label} must resolve to a string")),
    }
}
fn capture_tool_schemas(source: Option<&[JsValue]>) -> Result<Vec<JsValue>, String> {
    let Some(source) = source else {
        return Ok(Vec::new());
    };
    let mut schemas = source
        .iter()
        .map(clone_tool_schema)
        .collect::<Result<Vec<_>, _>>()?;
    schemas.sort_by(|left, right| {
        tool_name(left)
            .map(JsString::units)
            .unwrap_or_default()
            .cmp(tool_name(right).map(JsString::units).unwrap_or_default())
    });
    let mut names = std::collections::HashSet::new();
    for schema in &schemas {
        let name = tool_name(schema).cloned().unwrap_or_default();
        let display = String::from_utf16_lossy(name.units());
        if js_trim(&display).is_empty() {
            return Err("Prompt tool schema name must not be empty".into());
        }
        if !names.insert(name) {
            return Err(format!("Prompt tool schema \"{display}\" is duplicated"));
        }
    }
    Ok(schemas)
}
fn clone_tool_schema(schema: &JsValue) -> Result<JsValue, String> {
    let Some(object) = schema.as_object() else {
        return Err("Prompt tool schema must be an object".into());
    };
    let name = object
        .get("name")
        .and_then(JsValue::as_string)
        .ok_or_else(|| "Prompt tool schema name must not be empty".to_owned())?
        .clone();
    let mut cloned = JsObject::new();
    cloned.insert("name".into(), JsValue::String(name));
    if let Some(description) = object.get("description")
        && !description.is_undefined()
    {
        cloned.insert("description".into(), description.clone());
    }
    cloned.insert(
        "inputSchema".into(),
        object
            .get("inputSchema")
            .cloned()
            .unwrap_or(JsValue::Undefined),
    );
    Ok(JsValue::Object(cloned))
}
fn tool_name(schema: &JsValue) -> Option<&JsString> {
    schema
        .as_object()
        .and_then(|schema| schema.get("name"))
        .and_then(JsValue::as_string)
}

fn validate_contribution_name(name: &str, label: &str) -> Result<String, String> {
    let normalized = js_trim(name);
    if normalized.is_empty() {
        return Err(format!("{label} name must not be empty"));
    }
    if normalized != name {
        return Err(format!(
            "{label} name must not contain surrounding whitespace"
        ));
    }
    Ok(normalized.to_owned())
}
fn validate_order(order: f64, label: &str) -> Result<(), String> {
    if !order.is_finite() {
        return Err(format!("{label} order must be a finite number"));
    }
    Ok(())
}
fn valid_variable_name(name: &str) -> bool {
    let mut units = name.bytes();
    let Some(first) = units.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && units.all(|unit| unit.is_ascii_lowercase() || unit.is_ascii_digit() || unit == b'_')
}
fn compare_names(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
fn compare_sections(left: &SectionContribution, right: &SectionContribution) -> std::cmp::Ordering {
    left.order
        .partial_cmp(&right.order)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| compare_names(&left.name, &right.name))
}
fn compare_runtime_contexts(
    left: &RuntimeContextContribution,
    right: &RuntimeContextContribution,
) -> std::cmp::Ordering {
    left.order
        .partial_cmp(&right.order)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| compare_names(&left.name, &right.name))
}

fn interpolate_prompt_variables(
    text: &str,
    variables: &[(String, Option<String>)],
    owner: &str,
) -> Result<String, String> {
    let mut result = String::new();
    let mut last = 0usize;
    let mut open = text.find("{{");
    while let Some(at) = open {
        let rest = &text[at + 2..];
        let mut close = None;
        for (index, character) in rest.char_indices() {
            if character == '{' {
                break;
            }
            if character == '}' {
                if rest[index..].starts_with("}}") {
                    close = Some(index);
                }
                break;
            }
        }
        let Some(index) = close else {
            if rest.contains("}}") {
                return Err(format!("malformed prompt variable reference in {owner}"));
            }
            result.push_str(&text[last..at + 2]);
            last = at + 2;
            open = text[last..].find("{{").map(|offset| last + offset);
            continue;
        };
        let name = &rest[..index];
        if !valid_variable_name(name) {
            return Err(format!(
                "malformed prompt variable reference \"{{{{{name}}}}}\" in {owner}"
            ));
        }
        let Some((_, value)) = variables.iter().find(|(key, _)| key == name) else {
            let known = variables
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let known = if known.is_empty() {
                "(none)".to_owned()
            } else {
                known
            };
            return Err(format!(
                "unknown prompt variable \"{{{{{name}}}}}\" in {owner}; registered variables: {known}"
            ));
        };
        let Some(value) = value else {
            return Err(format!(
                "prompt variable \"{{{{{name}}}}}\" has no value in {owner}"
            ));
        };
        result.push_str(&text[last..at]);
        result.push_str(value);
        last = at + index + 4;
        open = text[last..].find("{{").map(|offset| last + offset);
    }
    result.push_str(&text[last..]);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn iso_dates_match_node_utc_slices() {
        assert_eq!(iso_date(0.0), "1970-01-01");
        assert_eq!(iso_date(1_749_484_800_000.0), "2025-06-09");
        assert_eq!(iso_date(-1.0), "1969-12-31");
    }
    #[test]
    fn iso_timestamps_match_node_utc_milliseconds() {
        assert_eq!(iso_timestamp(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_timestamp(1.0), "1970-01-01T00:00:00.001Z");
        assert_eq!(iso_timestamp(-1.0), "1969-12-31T23:59:59.999Z");
        assert_eq!(
            iso_timestamp(1_749_484_800_000.0),
            "2025-06-09T16:00:00.000Z"
        );
    }
    #[test]
    fn default_platform_uses_node_names() {
        if cfg!(target_os = "macos") {
            assert_eq!(process_platform(), "darwin");
        } else if cfg!(target_os = "linux") {
            assert_eq!(process_platform(), "linux");
        }
    }
}
