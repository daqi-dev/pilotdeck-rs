//! Phase-scoped registration of the four Always-On tools.
use std::sync::{Arc, Weak, atomic::{AtomicBool, Ordering}};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use crate::{host::{HostError, ModuleError}, host::modules::capability::ToolDefinition,
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRegistration, ToolRegistry, ToolRunContext}};
use super::manager::AlwaysOnRunContextRegistry;

/// A concrete phase owner implements these operations against its pinned
/// stores and workspace. The handler is never shared across phase sessions.
pub(crate) trait PhaseToolHandler: Send + Sync {
    fn execute(&self, name: &'static str, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput>;
}
pub(crate) struct PhaseHandlerEntry {
    pub active: Arc<AtomicBool>,
    pub handler: Arc<dyn PhaseToolHandler>,
}
pub(crate) struct PhaseHandlerLease {
    pub registry: Weak<AlwaysOnRunContextRegistry>,
    pub session: JsString,
    pub entry: Arc<PhaseHandlerEntry>,
}
impl Drop for PhaseHandlerLease {
    fn drop(&mut self) {
        self.entry.active.store(false, Ordering::Release);
        if let Some(registry) = self.registry.upgrade() {
            let mut entries = registry.tool_handlers.lock().unwrap_or_else(|e| e.into_inner());
            if entries.get(&self.session).is_some_and(|entry| Arc::ptr_eq(entry, &self.entry)) {
                entries.remove(&self.session);
            }
        }
    }
}
impl AlwaysOnRunContextRegistry {
    pub(crate) fn install_tool_handler(self: &Arc<Self>, session: JsString,
        handler: Arc<dyn PhaseToolHandler>) -> Result<PhaseHandlerLease, HostError> {
        let key = session.to_utf8().map_err(|_| ModuleError::new("Invalid Always-On phase session"))?;
        let contexts = self.contexts.lock().unwrap_or_else(|e| e.into_inner());
        if !contexts.contains_key(&key) { return Err(ModuleError::new("Always-On run context is not registered").into()); }
        let mut entries = self.tool_handlers.lock().unwrap_or_else(|e| e.into_inner());
        if entries.contains_key(&session) { return Err(ModuleError::new("Always-On phase handler already installed").into()); }
        let entry = Arc::new(PhaseHandlerEntry { active: Arc::new(AtomicBool::new(true)), handler });
        entries.insert(session.clone(), entry.clone());
        Ok(PhaseHandlerLease { registry: Arc::downgrade(self), session, entry })
    }
}

struct PhaseTool { fields: JsObject, name: &'static str, read_only: bool, entry: Arc<PhaseHandlerEntry> }
impl ToolDefinition for PhaseTool {
    fn fields(&self) -> &JsObject { &self.fields }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(vec!["always_on_run_context".into()])
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> { Ok(JsValue::Bool(self.read_only)) }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> { Ok(JsValue::Bool(self.read_only)) }
}
impl ExecutableTool for PhaseTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let entry = self.entry.clone(); let name = self.name;
        Box::pin(async move {
            if !entry.active.load(Ordering::Acquire) { return Err(ToolError::failed("Always-On phase is no longer active")); }
            if context.is_aborted() { return Err(ToolError::aborted()); }
            entry.handler.execute(name, input, context).await
        })
    }
}

pub(crate) fn register_tools(registry: &ToolRegistry, contexts: Arc<AlwaysOnRunContextRegistry>,
    session: &JsString) -> Result<Vec<ToolRegistration>, HostError> {
    let entry = contexts.tool_handlers.lock().unwrap_or_else(|e| e.into_inner())
        .get(session).cloned().ok_or_else(|| ModuleError::new("Always-On phase handler is not installed"))?;
    if !entry.active.load(Ordering::Acquire) { return Err(ModuleError::new("Always-On phase is no longer active").into()); }
    let definitions = [
        ("always_on_discovery_plan", "AlwaysOnDiscoveryPlan", false, "Save the single validated plan for this discovery run.", r#"{"type":"object","required":["title","summary","rationale","dedupeKey","content"],"additionalProperties":false,"properties":{"title":{"type":"string"},"summary":{"type":"string"},"rationale":{"type":"string"},"dedupeKey":{"type":"string"},"content":{"type":"string"}}}"#),
        ("always_on_prepare_workspace", "AlwaysOnPrepareWorkspace", false, "Create an isolated workspace for the plan using the chosen strategy.", r#"{"type":"object","required":["strategy"],"additionalProperties":false,"properties":{"strategy":{"type":"string","enum":["git-worktree","snapshot-copy","auto"]}}}"#),
        ("always_on_report", "AlwaysOnReport", false, "Save a markdown report of the current execution.", r#"{"type":"object","required":["content"],"additionalProperties":false,"properties":{"content":{"type":"string"}}}"#),
        ("always_on_read_chat_history", "AlwaysOnReadChatHistory", true, "Read a chat session identified in this discovery run.", r#"{"type":"object","required":["sessionId"],"additionalProperties":false,"properties":{"sessionId":{"type":"string"}}}"#),
    ];
    let mut registered: Vec<ToolRegistration> = Vec::new();
    for (name, alias, read_only, description, schema) in definitions {
        let mut fields = JsObject::new();
        fields.insert("name".into(), JsValue::String(name.into()));
        fields.insert("aliases".into(), JsValue::Array(vec![alias.into()]));
        fields.insert("kind".into(), JsValue::String("session".into()));
        fields.insert("description".into(), JsValue::String(description.into()));
        fields.insert("requiredRuntimeCapabilities".into(), JsValue::Array(vec!["always_on_run_context".into()]));
        fields.insert("inputSchema".into(), pilotdeck_protocol::parse_json(schema).expect("static Always-On schema"));
        match registry.register(Arc::new(PhaseTool { fields, name, read_only, entry: entry.clone() })) {
            Ok(handle) => registered.push(handle),
            Err(error) => { for handle in &registered { handle.dispose(); } return Err(error); },
        }
    }
    Ok(registered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    use std::sync::Mutex;

    struct Handler(Mutex<Vec<&'static str>>);
    impl PhaseToolHandler for Handler {
        fn execute(&self, name: &'static str, _: JsValue, _: ToolRunContext) -> ToolFuture<ToolOutput> {
            self.0.lock().unwrap().push(name);
            Box::pin(async { Ok(ToolOutput::default()) })
        }
    }
    fn context() -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(), files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None, model: None, elicitation: None, user_dialog: None,
            audit_recorder: None, file_history: None, file_update_notifier: None,
            now: None, progress: None, subagent: None, plan_todo: None,
            goal: None, plan_directory: None,
        }))
    }
    #[tokio::test]
    async fn registered_tool_dispatches_to_exact_live_phase_then_revokes() {
        let contexts = Arc::new(AlwaysOnRunContextRegistry::new());
        let session: JsString = "always-on/report:project=/p:run=r".into();
        contexts.register(super::super::manager::AlwaysOnRunContext {
            session_key: session.to_utf8().unwrap(), run_id: "r".into(), project_key: "/p".into(),
        }).unwrap();
        let handler = Arc::new(Handler(Mutex::default()));
        let lease = contexts.install_tool_handler(session.clone(), handler.clone()).unwrap();
        assert!(contexts.install_tool_handler(session.clone(), handler.clone()).is_err());
        let registry = ToolRegistry::default();
        let handles = register_tools(&registry, contexts.clone(), &session).unwrap();
        assert_eq!(handles.len(), 4);
        let report = registry.get(&"always_on_report".into()).expect("report registered");
        let descriptor = crate::host::modules::capability::serialize_tool_descriptor(report.as_ref());
        assert_eq!(descriptor.get("requiredRuntimeCapabilities"),
            Some(&JsValue::Array(vec!["always_on_run_context".into()])));
        report.execute(JsValue::Object(JsObject::new()), context()).await.unwrap();
        assert_eq!(*handler.0.lock().unwrap(), ["always_on_report"]);
        contexts.unregister(&session.to_utf8().unwrap());
        assert!(report.execute(JsValue::Object(JsObject::new()), context()).await.is_err());
        drop(lease);
        assert!(register_tools(&registry, contexts, &session).is_err());
    }
}
