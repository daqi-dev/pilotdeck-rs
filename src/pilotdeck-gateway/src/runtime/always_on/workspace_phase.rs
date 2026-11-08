//! Workspace phase: reuse an active authorized cycle, or run the workspace
//! tool in a phase-owned session and persist its resulting cycle.
use std::sync::Arc;
use pilotdeck_protocol::{JsString, JsValue};
use super::{cycles::{WorkCycleRecord, WorkCycleStore}, manager::{AlwaysOnAgentGateway,
    AlwaysOnRunContext, AlwaysOnRunContextRegistry, ControlError, PhaseObserver, PhaseRequest},
    phase_tools::PhaseHandlers, state::DiscoveryStateStore, storage,
    workspace::{NativeWorkspace, WorkspaceHandle}, workspace_tool::{WorkspaceHandler, WorkspacePending, WorkspaceState}};

pub(crate) struct WorkspacePhase {
    pub project: JsString,
    pub gateway: Arc<dyn AlwaysOnAgentGateway>,
    pub contexts: Arc<AlwaysOnRunContextRegistry>,
    pub observer: Arc<dyn PhaseObserver>,
    pub workspace: Arc<NativeWorkspace>,
    pub cycles: Arc<WorkCycleStore>,
    pub state: Arc<DiscoveryStateStore>,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub uuid: Arc<dyn Fn() -> String + Send + Sync>,
    pub chinese: bool,
}
pub(crate) struct WorkspacePhaseResult { pub handle: WorkspaceHandle, pub cycle: WorkCycleRecord, pub reused: bool }
fn failure(code: &str, message: impl Into<String>) -> ControlError { ControlError { code: code.into(), message: message.into() } }
fn native_error(error: super::workspace::WorkspaceError) -> ControlError {
    failure(&error.code, String::from_utf16_lossy(error.message.units()))
}
struct ContextGuard { registry: Arc<AlwaysOnRunContextRegistry>, session: String }
impl Drop for ContextGuard {
    fn drop(&mut self) { self.registry.unregister(&self.session); }
}
impl WorkspacePhase {
    async fn cleanup_uncommitted(&self, handle: Option<WorkspaceHandle>, mut error: ControlError) -> ControlError {
        if let Some(handle) = handle {
            if handle.created_identity.is_none() {
                error.message.push_str("; workspace was not created by this phase; retained.");
            } else if let Err(cleanup) = self.workspace.dispose(handle, false).await {
                error.message = format!("{}; owned workspace cleanup failed: {}", error.message, String::from_utf16_lossy(cleanup.message.units()));
                error.code = "workspace_cleanup_failed".into();
            }
        }
        error
    }
    pub async fn run(&self, run_id: &str, title: JsString, state: &JsValue)
        -> Result<WorkspacePhaseResult, ControlError> {
        let project = self.project.to_utf8().map_err(|_| failure("invalid_project", "Project path is not representable"))?;
        if let Some(cycle_id) = state.get("activeWorkCycleId").and_then(JsValue::as_string) {
            let cycle_id = cycle_id.clone(); let cycles = self.cycles.clone();
            let cycle = storage::run(move || cycles.get_record_js(&cycle_id)).await
                .map_err(|e| failure("workspace_prepare_failed", e.to_string()))?;
            if let Some(cycle) = cycle.filter(|c| c.status == "active") {
                // NativeWorkspace performs project/owner/descriptor checks;
                // it must distinguish missing from an unsafe workspace.
                match self.workspace.authorize_cycle(cycle.clone()).await {
                    Ok(handle) => return Ok(WorkspacePhaseResult { handle, cycle, reused: true }),
                    Err(error) if error.is_missing() => {},
                    Err(error) => return Err(native_error(error)),
                }
            }
        }
        let session = super::derive_workspace_session_key(&project, run_id);
        self.contexts.register(AlwaysOnRunContext { session_key: session.clone(), run_id: run_id.into(), project_key: project.clone() })
            .map_err(|e| failure("workspace_prepare_failed", e.to_string()))?;
        let context_guard = ContextGuard { registry: self.contexts.clone(), session: session.clone() };
        let state_ref = Arc::new(tokio::sync::Mutex::new(WorkspaceState::default()));
        let handler = Arc::new(WorkspaceHandler { run_id: run_id.into(), title: title.clone(),
            workspace: self.workspace.clone(), state: state_ref.clone(), pending: Arc::<WorkspacePending>::default() });
        let handlers = Arc::new(PhaseHandlers::Workspace(handler));
        let lease = handlers.install(&self.contexts, session.as_str().into())
            .map_err(|e| failure("workspace_prepare_failed", e.to_string()))?;
        let mut prompt = if self.chinese { "为该计划创建隔离工作区：".encode_utf16().collect::<Vec<_>>() }
            else { "Prepare an isolated workspace for this plan: ".encode_utf16().collect::<Vec<_>>() };
        prompt.extend_from_slice(title.units());
        prompt.extend(format!("\nProject root: {project}\nRun: {run_id}\nUse always_on_prepare_workspace to create the workspace.").encode_utf16());
        let request = PhaseRequest { project_key: self.project.clone(), session_key: session.as_str().into(),
            channel_key: "always-on/workspace".into(), run_id: format!("{run_id}.workspace").as_str().into(),
            message: JsString::from_utf16(prompt), cwd: project.as_str().into(), permission_mode: "bypassPermissions".into(),
            bypass_available: true, can_prompt: false,
            exclude_tools: crate::config::session_overrides::UNATTENDED_SESSION_EXCLUDED_TOOLS.into_iter().map(JsString::from).collect(),
            permission_deny: Vec::new(), context_registry: self.contexts.clone(), register_automation_tools: true,
            observer: self.observer.clone() };
        let submitted = self.gateway.run_phase(request).await;
        drop(lease); drop(context_guard);
        handlers.drain().await;
        let mut result = match submitted {
            Ok(result) => result,
            Err(error) => {
                let handle = state_ref.lock().await.handle.take();
                return Err(self.cleanup_uncommitted(handle, error).await);
            }
        };
        result.finish().await;
        let created = state_ref.lock().await.handle.clone();
        let (handle, reused) = match created {
            Some(handle) => (handle, false),
            None => {
                let legacy = state.get("currentWorkspace").and_then(JsValue::as_object).cloned();
                if let Some(legacy) = legacy {
                    match self.workspace.authorize_legacy(legacy).await {
                        Ok(handle) => (handle, true),
                        Err(error) if error.is_missing() => (self.workspace.prepare("auto", run_id.into(), title, None).await.map_err(native_error)?, false),
                        Err(error) => return Err(native_error(error)),
                    }
                } else {
                    (self.workspace.prepare("auto", run_id.into(), title, None).await.map_err(native_error)?, false)
                }
            },
        };
        let cycle_id = (self.uuid)(); let now = (self.now)();
        let cycles = self.cycles.clone(); let state_store = self.state.clone();
        let run = handle.run_id.clone(); let id = cycle_id.clone(); let workspace = handle.cycle_workspace();
        let preserve = Arc::new(std::sync::atomic::AtomicBool::new(reused));
        let committed = preserve.clone(); let selected_cwd = handle.cwd.clone();
        let mut uncertain = false;
        let cycle = storage::run(move || {
            let cycle = match cycles.create(&workspace, run, id.as_str().into(), now) {
                Ok(cycle) => { committed.store(true, std::sync::atomic::Ordering::Release); cycle },
                Err(error) => {
                    // Atomic rename may have committed before a later sync
                    // error. Never delete a durable or uncertain reference.
                    match cycles.references_workspace(&selected_cwd) {
                        Ok(false) => {},
                        Ok(true) => { committed.store(true, std::sync::atomic::Ordering::Release); },
                        Err(_) => { committed.store(true, std::sync::atomic::Ordering::Release); uncertain = true; },
                    }
                    return Ok((Err(error), uncertain));
                }
            };
            let outcome = state_store.set_active_work_cycle_id(&id, now).map(|_| cycle);
            Ok((outcome, uncertain))
        }).await;
        let cycle = match cycle {
            Ok((Ok(cycle), _)) => cycle,
            Ok((Err(error), uncertain)) => {
                let mut error = failure("workspace_prepare_failed", error.to_string());
                if preserve.load(std::sync::atomic::Ordering::Acquire) {
                    error.message.push_str(if uncertain { "; cycle persistence is uncertain; workspace retained." } else { "; durable cycle owns workspace; workspace retained." });
                    return Err(error);
                }
                return Err(self.cleanup_uncommitted(Some(handle), error).await);
            }
            Err(error) => {
                // A failed blocking task provides no definitive transaction
                // result. Preserve rather than guessing that rename failed.
                return Err(failure("workspace_prepare_failed", format!("{error}; persistence outcome unknown; workspace retained.")));
            }
        };
        Ok(WorkspacePhaseResult { handle, cycle, reused })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
        runtime::{project::{ProjectRuntimeRegistry, ResourceBundle}, tools::{ToolRegistry, ToolRunContext,
            builtin::bash::runner::NativeOwnedShellCommandRunner}}};
    use std::{future::Future, path::Path, pin::Pin};

    pub(super) struct Gateway;
    impl AlwaysOnAgentGateway for Gateway {
        fn abort_turn<'a>(&'a self, _: super::super::manager::GatewayAbortInput<'a>)
            -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>> { Box::pin(async { Ok(()) }) }
        fn run_phase<'a>(&'a self, request: PhaseRequest)
            -> Pin<Box<dyn Future<Output = Result<super::super::manager::PhaseResult, ControlError>> + Send + 'a>> {
            Box::pin(async move {
                let registry = ToolRegistry::default();
                super::super::tools::register_tools(&registry, request.context_registry, &request.session_key)
                    .map_err(|error| failure("tool_registry", error.to_string()))?;
                let tool = registry.get(&"always_on_prepare_workspace".into()).expect("workspace tool");
                let host = ToolRuntimeContext { fields: Default::default(), files: HostToolCheckpoint::default().tool_context_state(),
                    abort_signal: None, model: None, elicitation: None, user_dialog: None, audit_recorder: None,
                    file_history: None, file_update_notifier: None, now: None, progress: None, subagent: None,
                    plan_todo: None, goal: None, plan_directory: None };
                let mut input = pilotdeck_protocol::JsObject::new(); input.insert("strategy".into(), "snapshot-copy".into());
                tool.execute(JsValue::Object(input), ToolRunContext::new(Arc::new(host))).await
                    .map_err(|error| failure(&error.code, String::from_utf16_lossy(error.message.units())))?;
                let mut terminal = pilotdeck_protocol::JsObject::new(); terminal.insert("type".into(), "turn_completed".into());
                Ok(super::super::manager::PhaseResult::new(vec![JsValue::Object(terminal)], None))
            })
        }
    }
    pub(super) struct Observer;
    impl PhaseObserver for Observer {
        fn on_event<'a>(&'a self, _: &'a JsString, _: &'a JsString, _: &'a JsString, _: &'a JsValue)
            -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> { Box::pin(async {}) }
    }
    #[tokio::test]
    async fn workspace_turn_creates_real_snapshot_cycle_and_state() {
        let root = std::env::temp_dir().join(format!("pdrs-workspace-phase-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap(); let root = root.canonicalize().unwrap();
        let project = root.join("project"); let home = root.join("home");
        std::fs::create_dir_all(&project).unwrap(); std::fs::create_dir_all(&home).unwrap();
        std::fs::write(project.join("fixture.txt"), b"owned workspace\n").unwrap();
        let md = crate::metadata::Metadata { home: home.clone(), cwd: Default::default(), files: crate::metadata::NativeFiles };
        let paths = super::super::resolve_always_on_paths(&home, project.to_str().unwrap(), &md).unwrap();
        let registry = Arc::new(ProjectRuntimeRegistry::new(project.clone(), Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(()))));
        let lease = Arc::new(registry.acquire(None).unwrap());
        let env = Some(vec![("PATH".into(), "/usr/bin:/bin".into()), ("HOME".into(), home.to_str().unwrap().into()),
            ("PILOTDECK_SHELL_PATH".into(), "/bin/sh".into())]);
        let provider = Arc::new(NativeWorkspace::new(paths.clone(), lease, Arc::new(NativeOwnedShellCommandRunner), env).unwrap());
        let cycles = Arc::new(WorkCycleStore::new(paths.clone())); let state = Arc::new(DiscoveryStateStore::new(paths));
        let phase = WorkspacePhase { project: project.to_str().unwrap().into(), gateway: Arc::new(Gateway),
            contexts: Arc::new(AlwaysOnRunContextRegistry::new()), observer: Arc::new(Observer), workspace: provider.clone(),
            cycles: cycles.clone(), state: state.clone(), now: Arc::new(|| 0.0), uuid: Arc::new(|| "cycle".into()), chinese: false };
        let result = phase.run("run", "title".into(), &JsValue::Object(Default::default())).await.unwrap();
        assert!(!result.reused); assert!(result.cycle.id.equals_str("cycle"));
        let cwd = result.handle.cwd.to_utf8().unwrap();
        assert_eq!(std::fs::read(Path::new(&cwd).join("fixture.txt")).unwrap(), b"owned workspace\n");
        assert!(cycles.read_apply("cycle".into()).await.unwrap().is_some());
        assert!(state.read_async(0.0).await.unwrap().get("activeWorkCycleId").and_then(JsValue::as_string).unwrap().equals_str("cycle"));
        provider.dispose(result.handle, false).await.unwrap(); provider.close().await.unwrap();
        registry.dispose().await.unwrap(); std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod failure_cleanup_tests {
    use super::*;
    use std::{future::Future, path::{Path, PathBuf}, pin::Pin, os::unix::fs::PermissionsExt};
    use crate::runtime::{project::{ProjectRuntimeRegistry, ResourceBundle}, tools::builtin::bash::runner::NativeOwnedShellCommandRunner};
    struct GatewayFault { paths: super::super::AlwaysOnPaths, fault: &'static str }
    impl AlwaysOnAgentGateway for GatewayFault {
        fn abort_turn<'a>(&'a self, _: super::super::manager::GatewayAbortInput<'a>) -> Pin<Box<dyn Future<Output=Result<(),JsString>>+Send+'a>> { Box::pin(async { Ok(()) }) }
        fn run_phase<'a>(&'a self, request: PhaseRequest) -> Pin<Box<dyn Future<Output=Result<super::super::manager::PhaseResult,ControlError>>+Send+'a>> {
            Box::pin(async move {
                let result = super::tests::Gateway.run_phase(request).await?;
                match self.fault {
                    "phase" => return Err(failure("injected_phase_failure", "phase failed after actual workspace tool success")),
                    "index" => std::fs::set_permissions(self.paths.cycle_index_file.parent().unwrap(), std::fs::Permissions::from_mode(0o500)).unwrap(),
                    "state" => std::fs::create_dir(&self.paths.state_file).unwrap(),
                    "corrupt" => { std::fs::write(&self.paths.cycle_index_file, b"{").unwrap(); std::fs::set_permissions(self.paths.cycle_index_file.parent().unwrap(), std::fs::Permissions::from_mode(0o500)).unwrap(); },
                    "schema" => { std::fs::write(&self.paths.cycle_index_file, br#"{"schemaVersion":2,"cycles":[]}"#).unwrap(); std::fs::set_permissions(self.paths.cycle_index_file.parent().unwrap(), std::fs::Permissions::from_mode(0o500)).unwrap(); },
                    _ => unreachable!(),
                }
                Ok(result)
            })
        }
    }
    struct Scratch(PathBuf);
    impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    async fn fixture(fault: &'static str) {
        let root = Scratch(std::env::temp_dir().join(format!("pdrs-workspace-cleanup-{}", uuid::Uuid::new_v4())));
        let project = root.0.join("project"); let home = root.0.join("home");
        std::fs::create_dir_all(&project).unwrap(); std::fs::create_dir_all(&home).unwrap(); std::fs::write(project.join("source"), "ORIGINAL").unwrap();
        let project = project.canonicalize().unwrap(); let home = home.canonicalize().unwrap();
        let md = crate::metadata::Metadata { home: home.clone(), cwd: Default::default(), files: crate::metadata::NativeFiles };
        let paths = super::super::resolve_always_on_paths(&home, project.to_str().unwrap(), &md).unwrap();
        std::fs::create_dir_all(paths.cycle_index_file.parent().unwrap()).unwrap();
        std::fs::write(&paths.cycle_index_file, r#"{"schemaVersion":1,"cycles":[]}"#).unwrap();
        let registry = Arc::new(ProjectRuntimeRegistry::new(project.clone(), Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(()))));
        let provider = Arc::new(NativeWorkspace::new(paths.clone(), Arc::new(registry.acquire(None).unwrap()), Arc::new(NativeOwnedShellCommandRunner), Some(vec![("PATH".into(), "/usr/bin:/bin".into()), ("HOME".into(), home.to_str().unwrap().into())])).unwrap());
        let cycles = Arc::new(WorkCycleStore::new(paths.clone()));
        let phase = WorkspacePhase { project: project.to_str().unwrap().into(), gateway: Arc::new(GatewayFault { paths: paths.clone(), fault }), contexts: Arc::new(AlwaysOnRunContextRegistry::new()), observer: Arc::new(super::tests::Observer), workspace: provider.clone(), cycles: cycles.clone(), state: Arc::new(DiscoveryStateStore::new(paths.clone())), now: Arc::new(||0.0), uuid: Arc::new(|| "cleanup-cycle".into()), chinese: false };
        let result = phase.run("cleanup-run", "fixture".into(), &JsValue::Object(Default::default())).await;
        if fault == "index" || fault == "schema" || fault == "corrupt" { std::fs::set_permissions(paths.cycle_index_file.parent().unwrap(), std::fs::Permissions::from_mode(0o700)).unwrap(); }
        let target = paths.snapshots_dir().join("cleanup-run"); let cycle = cycles.read_apply("cleanup-cycle".into()).await.unwrap();
        if fault == "corrupt" || fault == "schema" { assert!(cycles.references_workspace(&target.to_string_lossy().as_ref().into()).is_err()); }
        let closed = provider.close().await; let drained = registry.dispose().await;
        assert!(result.is_err()); assert!(closed.is_ok()); assert!(drained.is_ok());
        if fault == "state" {
            assert!(target.exists(), "a committed cycle must retain its actual workspace even when set-active fails");
            let cycle = cycle.unwrap().0; assert_eq!(cycle.workspace.unwrap().cwd.to_utf8().unwrap(), target.to_string_lossy());
            assert!(result.err().unwrap().message.contains("durable cycle owns workspace"));
        } else if fault == "corrupt" || fault == "schema" {
            assert!(target.exists(), "uncertain cycle persistence must retain the actual workspace");
            let message = result.err().unwrap().message;
            assert!(message.contains("cycle persistence is uncertain; workspace retained."), "{message}");
            assert!(!message.contains("durable cycle owns workspace"), "{message}");
        } else { assert!(cycle.is_none()); assert!(!target.exists(), "uncommitted newly created workspace must be reclaimed: {fault}"); }
        assert_eq!(std::fs::read(project.join("source")).unwrap(), b"ORIGINAL");
    }
    #[tokio::test] async fn phase_failure_reclaims_only_the_new_workspace() { fixture("phase").await; }
    #[tokio::test] async fn cycle_write_failure_reclaims_an_unreferenced_workspace() { fixture("index").await; }
    #[tokio::test] async fn set_active_failure_retains_the_already_durable_cycle_workspace() { fixture("state").await; }
    #[tokio::test] async fn corrupt_cycle_json_retains_workspace_with_uncertain_diagnostic() { fixture("corrupt").await; }
    #[tokio::test] async fn unknown_cycle_schema_retains_workspace_with_uncertain_diagnostic() { fixture("schema").await; }
}
