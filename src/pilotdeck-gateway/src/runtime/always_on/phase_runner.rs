//! One owner-bound Always-On phase and its lifetime fence.
use std::sync::Arc;
use pilotdeck_protocol::JsString;
use super::{manager::{AlwaysOnAgentGateway, AlwaysOnRunContext, AlwaysOnRunContextRegistry,
    ControlError, PhaseObserver, PhaseRequest, PhaseResult}, phase_tools::PhaseHandlers};

pub(crate) struct PhaseRunner {
    pub project: JsString,
    pub gateway: Arc<dyn AlwaysOnAgentGateway>,
    pub contexts: Arc<AlwaysOnRunContextRegistry>,
    pub observer: Arc<dyn PhaseObserver>,
}
pub(crate) struct PhaseSpec {
    pub run_id: String,
    pub session_key: String,
    pub channel_key: JsString,
    pub phase_run: JsString,
    pub message: JsString,
    pub cwd: std::path::PathBuf,
    pub deny: Vec<crate::config::permission::PermissionRule>,
    pub handlers: Arc<PhaseHandlers>,
}
struct ContextGuard { registry: Arc<AlwaysOnRunContextRegistry>, session: String }
impl Drop for ContextGuard {
    fn drop(&mut self) { self.registry.unregister(&self.session); }
}
fn failure(message: impl Into<String>) -> ControlError {
    ControlError { code: "internal".into(), message: message.into() }
}
impl PhaseRunner {
    pub(crate) async fn run(&self, spec: PhaseSpec) -> Result<PhaseResult, ControlError> {
        let project = self.project.to_utf8().map_err(|_| failure("Project path is not representable"))?;
        self.contexts.register(AlwaysOnRunContext { session_key: spec.session_key.clone(),
            run_id: spec.run_id, project_key: project }).map_err(|e| failure(e.to_string()))?;
        let context = ContextGuard { registry: self.contexts.clone(), session: spec.session_key.clone() };
        let lease = spec.handlers.install(&self.contexts, spec.session_key.as_str().into())
            .map_err(|e| failure(e.to_string()))?;
        let request = PhaseRequest { project_key: self.project.clone(), session_key: spec.session_key.as_str().into(),
            channel_key: spec.channel_key, run_id: spec.phase_run, message: spec.message, cwd: spec.cwd,
            permission_mode: "bypassPermissions".into(), bypass_available: true, can_prompt: false,
            exclude_tools: crate::config::session_overrides::UNATTENDED_SESSION_EXCLUDED_TOOLS.into_iter().map(JsString::from).collect(),
            permission_deny: spec.deny, context_registry: self.contexts.clone(),
            register_automation_tools: true, observer: self.observer.clone() };
        let result = self.gateway.run_phase(request).await;
        drop(lease); drop(context);
        spec.handlers.drain().await;
        let mut result = result?;
        result.finish().await;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, pin::Pin, sync::atomic::{AtomicBool, Ordering}};
    use pilotdeck_protocol::JsValue;
    use super::super::manager::{GatewayAbortInput, PhaseCompletion};
    struct Completion(Arc<AtomicBool>);
    impl PhaseCompletion for Completion {
        fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move { self.0.store(true, Ordering::SeqCst); })
        }
    }
    struct Gateway(Arc<AtomicBool>);
    impl AlwaysOnAgentGateway for Gateway {
        fn abort_turn<'a>(&'a self, _: GatewayAbortInput<'a>)
            -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>> { Box::pin(async { Ok(()) }) }
        fn run_phase<'a>(&'a self, request: PhaseRequest)
            -> Pin<Box<dyn Future<Output = Result<PhaseResult, ControlError>> + Send + 'a>> {
            let closed = self.0.clone();
            Box::pin(async move {
                assert!(request.context_registry.get("session").is_some());
                assert!(request.register_automation_tools && request.bypass_available && !request.can_prompt);
                assert!(request.permission_mode.equals_str("bypassPermissions"));
                let mut terminal = pilotdeck_protocol::JsObject::new(); terminal.insert("type".into(), "turn_completed".into());
                Ok(PhaseResult::new(vec![JsValue::Object(terminal)], None).with_completion(Box::new(Completion(closed))))
            })
        }
    }
    struct Observer;
    impl PhaseObserver for Observer {
        fn on_event<'a>(&'a self, _: &'a JsString, _: &'a JsString, _: &'a JsString, _: &'a JsValue)
            -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> { Box::pin(async {}) }
    }
    #[tokio::test]
    async fn phase_registers_and_revokes_context_before_closing_session() {
        let contexts = Arc::new(AlwaysOnRunContextRegistry::new());
        let closed = Arc::new(AtomicBool::new(false));
        let runner = PhaseRunner { project: "/project".into(), gateway: Arc::new(Gateway(closed.clone())),
            contexts: contexts.clone(), observer: Arc::new(Observer) };
        let result = runner.run(PhaseSpec { run_id: "run".into(), session_key: "session".into(),
            channel_key: "always-on/execute".into(), phase_run: "run.execute".into(), message: "work".into(),
            cwd: "/workspace".into(), deny: Vec::new(), handlers: Arc::new(PhaseHandlers::Execution) }).await.unwrap();
        assert_eq!(result.events.len(), 1);
        assert!(closed.load(Ordering::SeqCst));
        assert!(contexts.get("session").is_none());
    }
}
