//! Turn-independent session services shared by the native session factory.
//!
//! The channel-side cron runtime owns the always-on run tools and the durable
//! task owner; the runtime keeps those as host-injected ports so this crate
//! does not depend on the channel layer. Everything here is real state the
//! builtin tools read: the transcript writer, background resources, the
//! durable goal/todo services and the optional cron ports.

use crate::runtime::background::BackgroundResources;
use crate::runtime::tools::builtin::goal::DurableGoalService;
use crate::runtime::tools::builtin::todo_write::DurableTodoService;
use crate::storage::{
    AsyncTranscriptAdapter, JsonlTranscriptStore, SessionStorageInput, TranscriptWriter,
};

/// Host-injected cron surface used by session tools.
///
/// The channel layer supplies the real implementations: the durable task owner
/// and the always-on phase tools. Absent means those tools are not registered
/// for this session, which is what a build without the cron channel does.
pub trait NativeCronPort: Send + Sync {
    /// Durable cron task owner, registered as session tools when enabled.
    fn register_tools(
        &self,
        registry: &crate::runtime::tools::ToolRegistry,
    ) -> std::io::Result<()>;
}

/// The always-on phase tool surface selected by the running phase.
pub trait PhaseTools: Send + Sync {
    fn schemas(&self) -> Vec<serde_json::Value>;
    fn call<'a>(
        &'a self,
        name: &'a str,
        input: &'a serde_json::Value,
    ) -> crate::host::HostFuture<serde_json::Value>;
}

/// Turn-independent resources selected by the owning session factory.
pub struct NativeSessionServices {
    pub tools: Option<std::sync::Arc<dyn PhaseTools>>,
    pub background: BackgroundResources,
    pub writer: std::sync::Arc<TranscriptWriter>,
    pub goal: std::sync::Arc<DurableGoalService>,
    pub plan_todo: std::sync::Arc<DurableTodoService>,
    pub cron: Option<std::sync::Arc<dyn NativeCronPort>>,
}

impl NativeSessionServices {
    /// Build the transcript writer and the durable goal/todo roots for one
    /// session. The chat directory comes from the same layout the rest of the
    /// gateway uses, so restoring the session reads the file the factory owns.
    pub fn open(
        home: &std::path::Path,
        cwd: &std::path::Path,
        project: &str,
        session: &pilotdeck_protocol::JsString,
        tools: Option<std::sync::Arc<dyn PhaseTools>>,
        background: BackgroundResources,
        cron: Option<std::sync::Arc<dyn NativeCronPort>>,
    ) -> std::io::Result<Self> {
        let chat_dir = crate::session_chat_dir(home, cwd, project);
        let adapter = AsyncTranscriptAdapter::new(
            std::sync::Arc::new(JsonlTranscriptStore::new()),
            std::sync::Arc::new({
                let cwd = cwd.to_path_buf();
                move |key| {
                    crate::session_chat_dir(
                        &key.pilot_home,
                        &cwd,
                        &key.project_root.to_string_lossy(),
                    )
                }
            }),
        );
        let storage = adapter
            .create_session_storage(SessionStorageInput {
                pilot_home: home.to_path_buf(),
                project_root: project.into(),
                session_id: session.clone(),
                now: None,
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        storage
            .restore()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // Goal and todo state are session-scoped, but both services may be
        // shared by nested turns. Keep their durable roots next to the managed
        // transcript directory derived from the explicit home/cwd.
        let tool_state = chat_dir.join("tools");
        let goal = std::sync::Arc::new(DurableGoalService::open(tool_state.join("goals"))?);
        let plan_todo =
            std::sync::Arc::new(DurableTodoService::open(tool_state.join("todos"))?);
        Ok(Self {
            tools,
            background,
            writer: std::sync::Arc::new(storage.transcript),
            goal,
            plan_todo,
            cron,
        })
    }
}
