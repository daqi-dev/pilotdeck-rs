use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        context::{
            ContributedCommand, ContributedPrompt, ContributedSkill, ExtensionResolver,
            McpServerInstruction, memory::MemoryResolver,
        },
        plugins::{
            PluginRuntime, PluginSessionContributionLease, PluginSessionContributionSnapshot,
        },
        tools::{ExecutableTool, ToolFuture, ToolOutput, ToolRegistry, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
pub struct ContextResources {
    extension: Arc<SessionExtensionResolver>,
    owner: Option<Arc<ContextResourceOwner>>,
    native_memory: Option<ProjectMemoryLease>,
    child_scope: Option<Arc<ChildScope>>,
    pub memory_resolver: Option<Arc<dyn MemoryResolver>>,
    pub project_root: String,
    pub memory_retrieval_timeout_ms: f64,
}

impl ContextResources {
    pub fn empty(project_root: impl Into<String>) -> Self {
        Self {
            extension: Arc::new(SessionExtensionResolver::default()),
            owner: None,
            native_memory: None,
            child_scope: None,
            memory_resolver: None,
            project_root: project_root.into(),
            memory_retrieval_timeout_ms: 30_000.0,
        }
    }

    pub fn from_contributions(
        contributions: PluginSessionContributionSnapshot,
        project_root: impl Into<String>,
    ) -> Self {
        Self {
            extension: Arc::new(SessionExtensionResolver::from(contributions)),
            owner: None,
            native_memory: None,
            child_scope: None,
            memory_resolver: None,
            project_root: project_root.into(),
            memory_retrieval_timeout_ms: 30_000.0,
        }
    }

    pub fn from_owned_runtime_lease(
        runtime: Arc<PluginRuntime>,
        lease: PluginSessionContributionLease,
        project_root: impl Into<String>,
    ) -> Self {
        Self::from_runtime_lease(runtime, lease, project_root, true)
    }

    pub fn from_shared_runtime_lease(
        runtime: Arc<PluginRuntime>,
        lease: PluginSessionContributionLease,
        project_root: impl Into<String>,
    ) -> Self {
        Self::from_runtime_lease(runtime, lease, project_root, false)
    }

    fn from_runtime_lease(
        runtime: Arc<PluginRuntime>,
        lease: PluginSessionContributionLease,
        project_root: impl Into<String>,
        dispose_runtime: bool,
    ) -> Self {
        let extension = Arc::new(SessionExtensionResolver::from(lease.contributions.clone()));
        Self {
            extension,
            owner: Some(Arc::new(ContextResourceOwner {
                runtime,
                dispose_runtime,
                lease: tokio::sync::Mutex::new(Some(lease)),
                disposed: tokio::sync::OnceCell::new(),
                child_scopes: AtomicUsize::new(0),
                child_scopes_changed: tokio::sync::Notify::new(),
            })),
            native_memory: None,
            child_scope: None,
            memory_resolver: None,
            project_root: project_root.into(),
            memory_retrieval_timeout_ms: 30_000.0,
        }
    }

    pub fn with_memory_resolver(mut self, resolver: Option<Arc<dyn MemoryResolver>>) -> Self {
        self.memory_resolver = resolver;
        self
    }

    pub fn with_memory_retrieval_timeout_ms(mut self, timeout_ms: f64) -> Self {
        self.memory_retrieval_timeout_ms = timeout_ms;
        self
    }

    pub fn with_native_memory_owner(
        mut self,
        owner: crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner,
    ) -> Self {
        let lease = ProjectMemoryLease::standalone(owner);
        self.memory_resolver = Some(lease.resolver());
        self.memory_retrieval_timeout_ms = lease.retrieval_timeout_ms();
        self.native_memory = Some(lease);
        self
    }

    pub fn with_project_memory_lease(mut self, lease: ProjectMemoryLease) -> Self {
        self.memory_resolver = Some(lease.resolver());
        self.memory_retrieval_timeout_ms = lease.retrieval_timeout_ms();
        self.native_memory = Some(lease);
        self
    }

    pub fn extension(&self) -> Arc<dyn ExtensionResolver> {
        self.extension.clone()
    }

    pub fn for_subagent(&self, skills: Option<&[String]>, memory_disabled: bool) -> Self {
        let extension = match skills {
            Some(names) => Arc::new(self.extension.only_skills(names)),
            None => self.extension.clone(),
        };
        let child_scope = self.owner.as_ref().map(ContextResourceOwner::begin_child);
        Self {
            extension,
            owner: self.owner.clone(),
            native_memory: if memory_disabled {
                None
            } else {
                self.native_memory
                    .as_ref()
                    .map(ProjectMemoryLease::acquire_child)
            },
            child_scope,
            memory_resolver: if memory_disabled {
                None
            } else {
                self.memory_resolver.clone()
            },
            project_root: self.project_root.clone(),
            memory_retrieval_timeout_ms: self.memory_retrieval_timeout_ms,
        }
    }

    pub fn for_session_skills(&self, skills: Option<&[String]>) -> Self {
        match skills {
            Some(names) => Self {
                extension: Arc::new(self.extension.only_skills(names)),
                owner: self.owner.clone(),
                native_memory: self.native_memory.clone(),
                child_scope: self.child_scope.clone(),
                memory_resolver: self.memory_resolver.clone(),
                project_root: self.project_root.clone(),
                memory_retrieval_timeout_ms: self.memory_retrieval_timeout_ms,
            },
            None => self.clone(),
        }
    }

    pub fn install_read_skill(&self, registry: &ToolRegistry) -> Result<(), HostError> {
        if self.extension.skills.is_empty() {
            return Ok(());
        }
        self.install_scoped_read_skill(registry)
    }

    pub fn install_scoped_read_skill(&self, registry: &ToolRegistry) -> Result<(), HostError> {
        registry.register_or_replace(Arc::new(ReadSkillTool::new(self.extension.clone())))?;
        Ok(())
    }

    pub async fn dispose_owner(&self) -> Result<(), String> {
        let plugin = match &self.owner {
            Some(owner) => owner.dispose().await,
            None => Ok(()),
        };
        let memory = match &self.native_memory {
            Some(owner) => owner.release().await,
            None => Ok(()),
        };
        match (plugin, memory) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(plugin), Err(memory)) => Err(format!(
                "Failed to dispose plugin resources: {plugin}; failed to dispose memory resources: {memory}"
            )),
        }
    }
}

struct NativeMemoryGeneration {
    provider: crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner,
    maintenance: crate::runtime::memory::ProjectMemoryMaintenanceController,
    leases: AtomicUsize,
    retired: AtomicBool,
    disposed: tokio::sync::OnceCell<Result<(), String>>,
}

impl NativeMemoryGeneration {
    fn new(provider: crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner) -> Self {
        let maintenance = crate::runtime::memory::ProjectMemoryMaintenanceController::default();
        maintenance.start(
            Some(provider.maintenance.clone()),
            provider.enabled,
            provider.has_llm,
        );
        Self {
            provider,
            maintenance,
            leases: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
            disposed: tokio::sync::OnceCell::new(),
        }
    }

    fn acquire(generation: &Arc<Self>) -> ProjectMemoryLease {
        generation.leases.fetch_add(1, Ordering::AcqRel);
        ProjectMemoryLease {
            inner: Arc::new(ProjectMemoryLeaseInner {
                generation: generation.clone(),
                active: AtomicBool::new(true),
            }),
        }
    }

    fn release_lease(&self) -> bool {
        let previous = self.leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "project memory lease count underflow");
        previous == 1 && self.retired.load(Ordering::Acquire)
    }

    fn needs_dispose(&self) -> bool {
        self.retired.load(Ordering::Acquire)
            && self.leases.load(Ordering::Acquire) == 0
            && self.disposed.get().is_none()
    }

    fn dispose_detached(generation: Arc<Self>) {
        let dispose = async move {
            let _ = generation.dispose().await;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(dispose);
            return;
        }
        let _ = std::thread::Builder::new()
            .name("project-memory-dispose".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(dispose);
                }
            });
    }

    fn retire(&self) -> bool {
        self.retired.store(true, Ordering::Release);
        self.leases.load(Ordering::Acquire) == 0
    }

    async fn dispose(&self) -> Result<(), String> {
        self.disposed
            .get_or_init(|| async {
                self.maintenance.stop();
                (self.provider.disposer)()
                    .await
                    .map_err(|error| error.message)
            })
            .await
            .clone()
    }
}

struct ProjectMemoryLeaseInner {
    generation: Arc<NativeMemoryGeneration>,
    active: AtomicBool,
}

impl Drop for ProjectMemoryLeaseInner {
    fn drop(&mut self) {
        let should_dispose = if self.active.swap(false, Ordering::AcqRel) {
            self.generation.release_lease()
        } else {
            self.generation.needs_dispose()
        };
        if should_dispose {
            NativeMemoryGeneration::dispose_detached(self.generation.clone());
        }
    }
}

#[derive(Clone)]
pub struct ProjectMemoryLease {
    inner: Arc<ProjectMemoryLeaseInner>,
}

impl ProjectMemoryLease {
    fn standalone(owner: crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner) -> Self {
        let generation = Arc::new(NativeMemoryGeneration::new(owner));
        generation.retired.store(true, Ordering::Release);
        NativeMemoryGeneration::acquire(&generation)
    }

    fn resolver(&self) -> Arc<dyn MemoryResolver> {
        self.inner.generation.provider.resolver.clone()
    }

    fn retrieval_timeout_ms(&self) -> f64 {
        self.inner.generation.provider.retrieval_timeout_ms
    }

    fn acquire_child(&self) -> Self {
        NativeMemoryGeneration::acquire(&self.inner.generation)
    }

    pub fn generation_id(&self) -> usize {
        Arc::as_ptr(&self.inner.generation) as usize
    }

    pub fn maintenance_service(&self) -> Arc<dyn crate::runtime::memory::MemoryMaintenanceService> {
        self.inner.generation.provider.maintenance.clone()
    }

    async fn release(&self) -> Result<(), String> {
        let should_dispose = if self.inner.active.swap(false, Ordering::AcqRel) {
            self.inner.generation.release_lease()
        } else {
            self.inner.generation.needs_dispose()
        };
        if should_dispose {
            self.inner.generation.dispose().await
        } else {
            Ok(())
        }
    }
}

struct ProjectMemoryEntry {
    fingerprint: String,
    generation: Option<Arc<NativeMemoryGeneration>>,
}

#[derive(Clone, Default)]
pub struct ProjectMemoryRegistry {
    entries: Arc<tokio::sync::Mutex<HashMap<String, ProjectMemoryEntry>>>,
}

impl ProjectMemoryRegistry {
    pub async fn acquire<F>(
        &self,
        project_key: String,
        fingerprint: String,
        create: F,
    ) -> Result<Option<ProjectMemoryLease>, String>
    where
        F: FnOnce() -> Result<
            Option<crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner>,
            String,
        >,
    {
        let mut entries = self.entries.lock().await;
        if let Some(current) = entries.get(&project_key)
            && current.fingerprint == fingerprint
        {
            return Ok(current
                .generation
                .as_ref()
                .map(NativeMemoryGeneration::acquire));
        }

        let next = create()?.map(|owner| Arc::new(NativeMemoryGeneration::new(owner)));
        let previous = entries.insert(
            project_key,
            ProjectMemoryEntry {
                fingerprint,
                generation: next.clone(),
            },
        );
        let lease = next.as_ref().map(NativeMemoryGeneration::acquire);
        drop(entries);
        if let Some(previous) = previous.and_then(|entry| entry.generation)
            && previous.retire()
        {
            previous.dispose().await?;
        }
        Ok(lease)
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let generations = {
            let mut entries = self.entries.lock().await;
            entries
                .drain()
                .filter_map(|(_, entry)| entry.generation)
                .collect::<Vec<_>>()
        };
        let mut failures = Vec::new();
        for generation in generations {
            if generation.retire()
                && let Err(error) = generation.dispose().await
            {
                failures.push(error);
            }
        }
        match failures.len() {
            0 => Ok(()),
            1 => Err(failures.remove(0)),
            _ => Err(format!(
                "Failed to dispose project memory generations: {}",
                failures.join("; ")
            )),
        }
    }

    pub fn resolve_published_runtime(
        &self,
        project_key: &str,
    ) -> Option<crate::runtime::memory::ProjectMemoryMaintenanceRuntime> {
        let entries = self.entries.try_lock().ok()?;
        let entry = entries.get(project_key)?;
        let generation = entry.generation.as_ref()?;
        if generation
            .retired
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        Some(crate::runtime::memory::ProjectMemoryMaintenanceRuntime {
            project_root: project_key.to_string(),
            generation_id: Arc::as_ptr(generation) as usize,
            memory_service: Some(generation.provider.maintenance.clone()),
        })
    }

    pub fn stage_published_owner(
        &self,
        project_key: &str,
        owner: crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner,
    ) -> (
        usize,
        Arc<dyn crate::runtime::memory::MemoryMaintenanceService>,
    ) {
        let service = owner.maintenance.clone();
        let generation = Arc::new(NativeMemoryGeneration::new(owner));
        let generation_id = Arc::as_ptr(&generation) as usize;
        if let Ok(mut entries) = self.entries.try_lock() {
            entries.insert(
                project_key.to_string(),
                ProjectMemoryEntry {
                    fingerprint: String::new(),
                    generation: Some(generation),
                },
            );
        }
        (generation_id, service)
    }

    #[cfg(test)]
    pub async fn current_generation_id(&self, project_key: &str) -> Option<usize> {
        self.entries
            .lock()
            .await
            .get(project_key)
            .and_then(|entry| entry.generation.as_ref())
            .map(|generation| Arc::as_ptr(generation) as usize)
    }
}

struct ContextResourceOwner {
    runtime: Arc<PluginRuntime>,
    dispose_runtime: bool,
    lease: tokio::sync::Mutex<Option<PluginSessionContributionLease>>,
    disposed: tokio::sync::OnceCell<Result<(), String>>,
    child_scopes: AtomicUsize,
    child_scopes_changed: tokio::sync::Notify,
}

impl ContextResourceOwner {
    fn begin_child(owner: &Arc<Self>) -> Arc<ChildScope> {
        owner.child_scopes.fetch_add(1, Ordering::AcqRel);
        Arc::new(ChildScope {
            owner: owner.clone(),
        })
    }

    async fn wait_for_children(&self) {
        loop {
            if self.child_scopes.load(Ordering::Acquire) == 0 {
                return;
            }
            let changed = self.child_scopes_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.child_scopes.load(Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }

    async fn dispose(&self) -> Result<(), String> {
        self.disposed
            .get_or_init(|| async {
                self.wait_for_children().await;
                let release = match self.lease.lock().await.take() {
                    Some(mut lease) => lease.release().await.map_err(|error| error.message),
                    None => Ok(()),
                };
                let dispose = if self.dispose_runtime {
                    self.runtime
                        .dispose()
                        .await
                        .map_err(|error| error.message)
                } else {
                    Ok(())
                };
                match (release, dispose) {
                    (Ok(()), Ok(())) => Ok(()),
                    (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                    (Err(release), Err(dispose)) => Err(format!(
                        "Failed to release plugin contributions: {release}; failed to dispose plugin runtime: {dispose}"
                    )),
                }
            })
            .await
            .clone()
    }
}

struct ChildScope {
    owner: Arc<ContextResourceOwner>,
}

impl Drop for ChildScope {
    fn drop(&mut self) {
        if self.owner.child_scopes.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.owner.child_scopes_changed.notify_waiters();
        }
    }
}

#[derive(Clone, Default)]
struct SessionExtensionResolver {
    commands: Vec<ContributedCommand>,
    skills: Vec<Skill>,
    mcp_instructions: Vec<McpServerInstruction>,
    prompts: Vec<ContributedPrompt>,
}

#[derive(Clone)]
struct Skill {
    contribution: ContributedSkill,
    content: String,
}

impl From<PluginSessionContributionSnapshot> for SessionExtensionResolver {
    fn from(value: PluginSessionContributionSnapshot) -> Self {
        Self {
            commands: value
                .commands
                .into_iter()
                .map(|command| ContributedCommand {
                    name: command.name,
                    description: command.description,
                    argument_hint: command.argument_hint,
                })
                .collect(),
            skills: value
                .skills
                .into_iter()
                .map(|skill| Skill {
                    contribution: ContributedSkill {
                        name: skill.name,
                        description: skill.description,
                        path: skill.path.to_string_lossy().into_owned(),
                    },
                    content: skill.content,
                })
                .collect(),
            mcp_instructions: value
                .mcp_instructions
                .into_iter()
                .map(|entry| McpServerInstruction {
                    server_name: entry.server_name,
                    instructions: Some(entry.instructions),
                })
                .collect(),
            prompts: value
                .prompts
                .into_iter()
                .enumerate()
                .map(|(index, content)| ContributedPrompt {
                    name: format!("plugin-prompt-{index}"),
                    content,
                })
                .collect(),
        }
    }
}

impl SessionExtensionResolver {
    fn only_skills(&self, names: &[String]) -> Self {
        let mut filtered = self.clone();
        filtered
            .skills
            .retain(|skill| names.iter().any(|name| name == &skill.contribution.name));
        filtered
    }
}

impl ExtensionResolver for SessionExtensionResolver {
    fn list_commands(&self) -> Vec<ContributedCommand> {
        self.commands.clone()
    }

    fn list_skills(&self) -> Vec<ContributedSkill> {
        self.skills
            .iter()
            .map(|skill| skill.contribution.clone())
            .collect()
    }

    fn list_mcp_instructions(&self) -> Vec<McpServerInstruction> {
        self.mcp_instructions.clone()
    }

    fn list_prompt_contributions(&self) -> Vec<ContributedPrompt> {
        self.prompts.clone()
    }
}

struct ReadSkillTool {
    fields: JsObject,
    extension: Arc<SessionExtensionResolver>,
}

impl ReadSkillTool {
    fn new(extension: Arc<SessionExtensionResolver>) -> Self {
        Self {
            fields: object([
                ("name", "read_skill".into()),
                ("aliases", JsValue::Array(vec!["ReadSkill".into()])),
                (
                    "description",
                    "Load a skill recipe by name and return its resolved SKILL.md path with the full content. Use this when the system prompt lists an available skill relevant to the current task.".into(),
                ),
                ("kind", "session".into()),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","required":["skillName"],"additionalProperties":false,"properties":{"skillName":{"type":"string","description":"The skill name as listed in <available-skills>."}}}"#,
                    )),
                ),
            ]),
            extension,
        }
    }

    fn response(&self, name: &str) -> String {
        match self
            .extension
            .skills
            .iter()
            .find(|skill| skill.contribution.name == name)
        {
            Some(skill) => format!(
                "<skill>\n<name>{}</name>\n<path>{}</path>\n{}\n</skill>",
                escape_xml(&skill.contribution.name),
                escape_xml(&skill.contribution.path),
                skill.content
            ),
            None if self.extension.skills.is_empty() => {
                format!("Skill '{name}' not found. No skills are currently loaded.")
            }
            None => format!(
                "Skill '{name}' not found. Available skills: {}",
                self.extension
                    .skills
                    .iter()
                    .map(|skill| skill.contribution.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl ToolDefinition for ReadSkillTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }

    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }

    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}

impl ExecutableTool for ReadSkillTool {
    fn execute(&self, input: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
        let name = input
            .get("skillName")
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
            .unwrap_or_default();
        let text = self.response(&name);
        Box::pin(async move {
            Ok(ToolOutput {
                content: vec![JsValue::Object(object([
                    ("type", "text".into()),
                    ("text", text.as_str().into()),
                ]))],
                ..Default::default()
            })
        })
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in entries {
        result.insert(key.into(), value);
    }
    result
}

fn schema(value: &str) -> JsObject {
    pilotdeck_protocol::parse_json(value)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .expect("read_skill schema is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::context::memory::{
        MemoryFuture, MemoryResolver, MemoryRetrieveInput, MemoryRetrieveResult,
    };
    use crate::runtime::memory::{
        MemoryFuture as MaintenanceFuture, MemoryMaintenanceService, MemoryManagementService,
        MemoryProviderError,
    };
    use crate::runtime::plugins::{
        LoadedPlugin, PluginRuntime, PluginRuntimeOptions, PluginSessionContributionSnapshot,
        PluginSkillContribution, PluginSource, loader::LoadedPluginCommand,
    };
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Memory;
    impl MemoryResolver for Memory {
        fn retrieve(&self, _: MemoryRetrieveInput) -> MemoryFuture {
            Box::pin(async { Ok(MemoryRetrieveResult::default()) })
        }
    }

    impl MemoryMaintenanceService for Memory {
        fn run_due_scheduled_maintenance(&self, _: &str) -> MaintenanceFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    impl MemoryManagementService for Memory {
        fn list(&self, _: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
            Ok(Vec::new())
        }

        fn clear(&self, _: &str) -> Result<(), MemoryProviderError> {
            Ok(())
        }

        fn clear_session(
            &self,
            _: &pilotdeck_protocol::JsString,
        ) -> Result<(), MemoryProviderError> {
            Ok(())
        }
    }

    fn resources() -> ContextResources {
        ContextResources::from_contributions(
            PluginSessionContributionSnapshot {
                generation: 7,
                tools: vec![],
                routers: vec![],
                commands: vec![],
                skills: vec![
                    PluginSkillContribution {
                        name: "review".into(),
                        description: Some("Review changes".into()),
                        path: "/project/.pilotdeck/skills/review/SKILL.md".into(),
                        content: "Review the changed code.".into(),
                        namespace: "project".into(),
                    },
                    PluginSkillContribution {
                        name: "release".into(),
                        description: None,
                        path: "/project/.pilotdeck/skills/release/SKILL.md".into(),
                        content: "Prepare the release.".into(),
                        namespace: "project".into(),
                    },
                ],
                prompts: vec![],
                hooks: Default::default(),
                mcp_servers: JsObject::new(),
                mcp_instructions: vec![],
            },
            "/project",
        )
        .with_memory_resolver(Some(Arc::new(Memory)))
        .with_memory_retrieval_timeout_ms(500.0)
    }

    fn plugin(name: &str, skill: &str) -> Arc<LoadedPlugin> {
        Arc::new(LoadedPlugin {
            name: name.into(),
            path: format!("/plugins/{name}").into(),
            source: PluginSource::Builtin,
            commands: vec![],
            skills: vec![LoadedPluginCommand {
                name: skill.into(),
                path: format!("/plugins/{name}/skills/{skill}/SKILL.md").into(),
                content: format!("{name} {skill} content"),
                frontmatter: HashMap::from([(
                    "description".into(),
                    serde_json::Value::String(format!("{name} {skill}")),
                )]),
                is_skill: true,
            }],
            output_styles: vec![],
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            router_contributions: Vec::new(),
            dispose: None,
        })
    }

    fn disposable_plugin(calls: Arc<AtomicUsize>) -> Arc<LoadedPlugin> {
        Arc::new(LoadedPlugin {
            name: "disposable".into(),
            path: "/plugins/disposable".into(),
            source: PluginSource::Builtin,
            commands: vec![],
            skills: vec![],
            output_styles: vec![],
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            router_contributions: Vec::new(),
            dispose: Some(Box::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            })),
        })
    }

    struct DummySpawner;
    impl super::super::sidecar_session::SidecarSpawner for DummySpawner {
        fn command(&self) -> std::io::Result<tokio::process::Command> {
            Ok(tokio::process::Command::new("/usr/bin/true"))
        }
    }

    #[tokio::test]
    async fn snapshot_installs_read_skill_and_projects_it_into_the_prompt() {
        let resources = resources();
        let registry = ToolRegistry::default();
        resources.install_read_skill(&registry).unwrap();
        let tool = registry.get(&"read_skill".into()).unwrap();
        let checkpoint = crate::host::modules::tool_context::HostToolCheckpoint::default();
        let context = ToolRunContext::new(Arc::new(
            crate::host::modules::tool_context::ToolRuntimeContext {
                fields: JsObject::new(),
                files: crate::host::modules::tool_context::ToolFileState {
                    read_file_state: checkpoint.read_file_state,
                    write_snapshots: checkpoint.write_snapshots,
                    allowed_read_files: vec![],
                },
                abort_signal: None,
                model: None,
                elicitation: None,
                user_dialog: None,
                audit_recorder: None,
                file_history: None,
                file_update_notifier: None,
                now: None,
                progress: None,
                subagent: None,
                plan_todo: None,
                goal: None,
                plan_directory: None,
            },
        ));
        let output = tool
            .execute(
                JsValue::Object(object([("skillName", "review".into())])),
                context,
            )
            .await
            .unwrap();
        assert_eq!(
            output.content[0].get("text"),
            Some(&"<skill>\n<name>review</name>\n<path>/project/.pilotdeck/skills/review/SKILL.md</path>\nReview the changed code.\n</skill>".into())
        );
        let prompt = crate::runtime::context::PromptAssembler::new(resources.extension())
            .assemble(
                "/project",
                "proof",
                "model",
                "default",
                None,
                &[],
                &[],
                None,
                false,
                None,
                false,
                false,
                0.0,
            )
            .unwrap();
        assert!(prompt.joined.contains("<available-skills>"));
        assert!(prompt.joined.contains(
            "- review — Review changes (file: /project/.pilotdeck/skills/review/SKILL.md)"
        ));
    }

    #[test]
    fn subagent_filters_skills_and_inherits_or_disables_memory() {
        let resources = resources();
        let inherited = resources.for_subagent(Some(&["review".into()]), false);
        assert!(inherited.memory_resolver.is_some());
        assert_eq!(
            inherited
                .extension()
                .list_skills()
                .into_iter()
                .map(|skill| skill.name)
                .collect::<Vec<_>>(),
            vec!["review"]
        );

        let disabled = resources.for_subagent(None, true);
        assert!(disabled.memory_resolver.is_none());
        assert_eq!(disabled.extension().list_skills().len(), 2);
        let filtered_tool = ReadSkillTool::new(inherited.extension.clone());
        assert_eq!(
            filtered_tool.response("release"),
            "Skill 'release' not found. Available skills: review"
        );
    }

    #[tokio::test]
    async fn sdk_plugin_skills_are_leased_and_child_scope_keeps_the_generation_alive() {
        let scratch = crate::scratch::Scratch::new("context-resource-lease");
        let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
            project_root: scratch.path().join("project"),
            pilot_home: scratch.path().join("home"),
            builtin_skills_root: None,
            builtin_plugins: vec![plugin("base", "base-skill")],
            builtin_plugins_enabled: HashMap::new(),
        }));
        runtime.refresh().await.unwrap();
        let lease = runtime
            .acquire_session_contributions(vec![plugin("sdk", "sdk-skill")])
            .await
            .unwrap();
        let resources = ContextResources::from_shared_runtime_lease(
            runtime.clone(),
            lease,
            scratch.path().to_string_lossy(),
        );
        let mut skills = resources
            .extension()
            .list_skills()
            .into_iter()
            .map(|skill| skill.name)
            .collect::<Vec<_>>();
        skills.sort();
        assert_eq!(skills, vec!["base-skill", "sdk-skill"]);

        let empty = resources.for_session_skills(Some(&[]));
        assert!(empty.extension().list_skills().is_empty());
        let registry = ToolRegistry::default();
        empty.install_read_skill(&registry).unwrap();
        assert!(!registry.has(&"read_skill".into()));

        let child = resources.for_subagent(Some(&["sdk-skill".into()]), false);
        drop(empty);
        let mut disposing = Box::pin(runtime.dispose());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), disposing.as_mut())
                .await
                .is_err(),
            "the parent resources must retain the plugin generation lease"
        );
        drop(resources);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), disposing.as_mut())
                .await
                .is_err(),
            "a child scope must retain the same plugin generation lease"
        );
        drop(child);
        tokio::time::timeout(std::time::Duration::from_secs(1), disposing)
            .await
            .expect("dropping the last context scope releases the lease")
            .unwrap();
    }

    #[tokio::test]
    async fn production_session_dispose_releases_lease_and_plugin_runtime() {
        let scratch = crate::scratch::Scratch::new("context-resource-owner");
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
            project_root: scratch.path().join("project"),
            pilot_home: scratch.path().join("home"),
            builtin_skills_root: None,
            builtin_plugins: vec![disposable_plugin(calls.clone())],
            builtin_plugins_enabled: HashMap::new(),
        }));
        runtime.refresh().await.unwrap();
        let lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
        let resources = Arc::new(ContextResources::from_owned_runtime_lease(
            runtime,
            lease,
            scratch.path().to_string_lossy(),
        ));
        let child_scope = resources.for_subagent(None, false);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let session = super::super::sidecar_session::SidecarSession::new(
            super::super::sidecar_session::SidecarSessionConfig {
                limits: crate::host::Limits::default(),
                session_key: "session".into(),
                project_key: None,
                cwd: scratch.path().to_string_lossy().as_ref().into(),
                snapshot: JsValue::Undefined,
                services: None,
            },
            Arc::new(DummySpawner),
        )
        .with_owned_context_resources(resources);
        let mut disposing = Box::pin(crate::runtime::session::LiveSession::dispose(&session));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), disposing.as_mut())
                .await
                .is_err(),
            "parent disposal must wait while a background child scope is alive"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(child_scope);
        tokio::time::timeout(std::time::Duration::from_secs(1), disposing)
            .await
            .expect("child completion unblocks parent resource disposal")
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        crate::runtime::session::LiveSession::dispose(&session)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn shared_project_runtime_survives_session_close_and_recreation() {
        let scratch = crate::scratch::Scratch::new("context-shared-runtime");
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
            project_root: scratch.path().join("project"),
            pilot_home: scratch.path().join("home"),
            builtin_skills_root: None,
            builtin_plugins: vec![disposable_plugin(calls.clone()), plugin("base", "shared")],
            builtin_plugins_enabled: HashMap::new(),
        }));
        runtime.refresh().await.unwrap();
        let session_a = ContextResources::from_shared_runtime_lease(
            runtime.clone(),
            runtime.acquire_session_contributions(vec![]).await.unwrap(),
            scratch.path().to_string_lossy(),
        );
        let session_b = ContextResources::from_shared_runtime_lease(
            runtime.clone(),
            runtime.acquire_session_contributions(vec![]).await.unwrap(),
            scratch.path().to_string_lossy(),
        );

        session_a.dispose_owner().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            runtime.lifecycle_state().await,
            crate::runtime::plugins::RegistryState::Active
        );
        assert!(
            session_b
                .extension()
                .list_skills()
                .iter()
                .any(|skill| skill.name == "shared")
        );

        let session_c = ContextResources::from_shared_runtime_lease(
            runtime.clone(),
            runtime
                .acquire_session_contributions(vec![])
                .await
                .expect("the cached project runtime remains usable for a new session"),
            scratch.path().to_string_lossy(),
        );
        session_b.dispose_owner().await.unwrap();
        session_c.dispose_owner().await.unwrap();
        runtime.dispose().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn child_scope_drop_cannot_lose_the_parent_dispose_wakeup() {
        let scratch = crate::scratch::Scratch::new("context-child-wakeup");
        let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
            project_root: scratch.path().join("project"),
            pilot_home: scratch.path().join("home"),
            builtin_skills_root: None,
            builtin_plugins: vec![plugin("base", "shared")],
            builtin_plugins_enabled: HashMap::new(),
        }));
        runtime.refresh().await.unwrap();
        for _ in 0..128 {
            let resources = Arc::new(ContextResources::from_shared_runtime_lease(
                runtime.clone(),
                runtime.acquire_session_contributions(vec![]).await.unwrap(),
                scratch.path().to_string_lossy(),
            ));
            let child = resources.for_subagent(None, false);
            let disposing = tokio::spawn({
                let resources = resources.clone();
                async move { resources.dispose_owner().await }
            });
            tokio::task::yield_now().await;
            drop(child);
            tokio::time::timeout(std::time::Duration::from_secs(1), disposing)
                .await
                .expect("child release must always wake parent disposal")
                .unwrap()
                .unwrap();
        }
        runtime.dispose().await.unwrap();
    }

    #[tokio::test]
    async fn native_memory_owner_waits_for_children_and_disposes_exactly_once() {
        let scratch = crate::scratch::Scratch::new("context-native-memory-owner");
        let runtime = Arc::new(PluginRuntime::new(PluginRuntimeOptions {
            project_root: scratch.path().join("project"),
            pilot_home: scratch.path().join("home"),
            builtin_skills_root: None,
            builtin_plugins: vec![],
            builtin_plugins_enabled: HashMap::new(),
        }));
        runtime.refresh().await.unwrap();
        let memory = Arc::new(Memory);
        let disposals = Arc::new(AtomicUsize::new(0));
        let dispose_count = disposals.clone();
        let resources = Arc::new(
            ContextResources::from_shared_runtime_lease(
                runtime.clone(),
                runtime.acquire_session_contributions(vec![]).await.unwrap(),
                scratch.path().to_string_lossy(),
            )
            .with_native_memory_owner(
                crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner {
                    resolver: memory.clone(),
                    maintenance: memory.clone(),
                    management: memory,
                    disposer: Arc::new(move || {
                        let dispose_count = dispose_count.clone();
                        Box::pin(async move {
                            dispose_count.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        })
                    }),
                    retrieval_timeout_ms: 321.0,
                    has_llm: false,
                    enabled: true,
                },
            ),
        );
        assert_eq!(resources.memory_retrieval_timeout_ms, 321.0);
        let child = resources.for_subagent(None, false);
        let mut disposing = Box::pin(resources.dispose_owner());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), disposing.as_mut())
                .await
                .is_err()
        );
        assert_eq!(disposals.load(Ordering::SeqCst), 0);
        drop(child);
        tokio::time::timeout(std::time::Duration::from_secs(1), disposing)
            .await
            .unwrap()
            .unwrap();
        resources.dispose_owner().await.unwrap();
        assert_eq!(disposals.load(Ordering::SeqCst), 1);
        runtime.dispose().await.unwrap();
    }

    fn native_owner(
        disposals: Arc<AtomicUsize>,
    ) -> crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner {
        let memory = Arc::new(Memory);
        crate::runtime::memory::edgeclaw::NativeProjectMemoryOwner {
            resolver: memory.clone(),
            maintenance: memory.clone(),
            management: memory,
            disposer: Arc::new(move || {
                let disposals = disposals.clone();
                Box::pin(async move {
                    disposals.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
            retrieval_timeout_ms: 321.0,
            has_llm: false,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn project_memory_registry_reuses_one_generation_and_reloads_after_leases_drain() {
        let registry = ProjectMemoryRegistry::default();
        let constructions = Arc::new(AtomicUsize::new(0));
        let old_disposals = Arc::new(AtomicUsize::new(0));
        let new_disposals = Arc::new(AtomicUsize::new(0));
        let first = registry
            .acquire("project".into(), "v1".into(), {
                let constructions = constructions.clone();
                let old_disposals = old_disposals.clone();
                move || {
                    constructions.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(native_owner(old_disposals)))
                }
            })
            .await
            .unwrap()
            .unwrap();
        let second = registry
            .acquire("project".into(), "v1".into(), {
                let constructions = constructions.clone();
                move || {
                    constructions.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(native_owner(Arc::new(AtomicUsize::new(0)))))
                }
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(constructions.load(Ordering::SeqCst), 1);
        assert_eq!(first.generation_id(), second.generation_id());

        let next = registry
            .acquire("project".into(), "v2".into(), {
                let constructions = constructions.clone();
                let new_disposals = new_disposals.clone();
                move || {
                    constructions.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(native_owner(new_disposals)))
                }
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(constructions.load(Ordering::SeqCst), 2);
        assert_ne!(first.generation_id(), next.generation_id());
        assert_eq!(old_disposals.load(Ordering::SeqCst), 0);
        first.release().await.unwrap();
        assert_eq!(old_disposals.load(Ordering::SeqCst), 0);
        second.release().await.unwrap();
        assert_eq!(old_disposals.load(Ordering::SeqCst), 1);
        second.release().await.unwrap();
        assert_eq!(old_disposals.load(Ordering::SeqCst), 1);

        registry.shutdown().await.unwrap();
        assert_eq!(new_disposals.load(Ordering::SeqCst), 0);
        next.release().await.unwrap();
        assert_eq!(new_disposals.load(Ordering::SeqCst), 1);
        registry.shutdown().await.unwrap();
        assert_eq!(new_disposals.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn project_memory_child_lease_keeps_retired_generation_alive() {
        let registry = ProjectMemoryRegistry::default();
        let disposals = Arc::new(AtomicUsize::new(0));
        let lease = registry
            .acquire("project".into(), "v1".into(), {
                let disposals = disposals.clone();
                move || Ok(Some(native_owner(disposals)))
            })
            .await
            .unwrap()
            .unwrap();
        let parent = ContextResources::empty("project").with_project_memory_lease(lease);
        let child = parent.for_subagent(None, false);
        registry.shutdown().await.unwrap();
        parent.dispose_owner().await.unwrap();
        assert_eq!(disposals.load(Ordering::SeqCst), 0);
        drop(child);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while disposals.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping the final child lease disposes the retired generation");
        assert_eq!(disposals.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn project_memory_disabled_child_does_not_hold_retired_generation() {
        let registry = ProjectMemoryRegistry::default();
        let disposals = Arc::new(AtomicUsize::new(0));
        let lease = registry
            .acquire("project".into(), "v1".into(), {
                let disposals = disposals.clone();
                move || Ok(Some(native_owner(disposals)))
            })
            .await
            .unwrap()
            .unwrap();
        let parent = ContextResources::empty("project").with_project_memory_lease(lease);
        let disabled = parent.for_subagent(None, true);
        assert!(disabled.memory_resolver.is_none());

        registry.shutdown().await.unwrap();
        parent.dispose_owner().await.unwrap();
        assert_eq!(disposals.load(Ordering::SeqCst), 1);
        drop(disabled);
        assert_eq!(disposals.load(Ordering::SeqCst), 1);
    }
}
