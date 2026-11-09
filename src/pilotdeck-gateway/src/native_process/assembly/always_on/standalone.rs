//! Standalone apply is available without an enabled discovery manager, using
//! the same registry, session owner, storage rules and native turn facade.
use super::NativeAlwaysOnGatewayBody;
use crate::runtime::{
    always_on::{
        AlwaysOnRunContextRegistry, ControlError, DiscoveryReportStore, NativeWorkspace,
        StandaloneApply, WorkCycleRecord, WorkCycleStore,
        apply::{ApplyPhase, ApplyWorkspace, WorkspaceDiff},
        event_sink::PhaseEventSink,
        standalone::{StandaloneProject, StandaloneProjectFactory, new_standalone},
    },
    project::{ProjectResources, ProjectRuntimeRegistry},
    tools::builtin::bash::{AbortSignal, runner::NativeOwnedShellCommandRunner},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

pub(crate) fn standalone_factory(
    project: Arc<ProjectRuntimeRegistry>,
    options: super::super::super::Options,
    env: crate::config::Env,
    gateway: Arc<NativeAlwaysOnGatewayBody>,
) -> Arc<dyn StandaloneApply> {
    new_standalone(Arc::new(Factory {
        project,
        options,
        env,
        gateway,
    }))
}
struct Factory {
    project: Arc<ProjectRuntimeRegistry>,
    options: super::super::super::Options,
    env: crate::config::Env,
    gateway: Arc<NativeAlwaysOnGatewayBody>,
}
fn failure(message: impl Into<String>) -> ControlError {
    ControlError {
        code: "gateway_request_failed".into(),
        message: message.into(),
    }
}
impl StandaloneProjectFactory for Factory {
    fn open<'a>(
        &'a self,
        project: &'a JsString,
    ) -> Pin<Box<dyn Future<Output = Result<StandaloneProject, ControlError>> + Send + 'a>> {
        Box::pin(async move {
            if self.gateway.shutdown.is_cancelled() {
                return Err(failure("Automation is stopping."));
            }
            let key = project
                .to_utf8()
                .map_err(|_| failure("Project is not a native path."))?;
            let home = self.options.home.clone();
            let registry = self.project.clone();
            let (paths, generation, chinese) = crate::runtime::always_on::storage::run(move || {
                let paths = super::bounded_paths(&home, &key)?;
                let lease = registry
                    .acquire(Some(std::path::Path::new(&key)))
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                let snapshot = lease
                    .runtime()
                    .resources()
                    .get::<ProjectResources>()
                    .ok_or_else(|| std::io::Error::other("Missing native project resources."))?
                    .context()
                    .snapshot();
                let chinese = snapshot
                    .get("config")
                    .and_then(|v| v.get("alwaysOn"))
                    .and_then(|v| v.get("language"))
                    .and_then(JsValue::as_string)
                    .and_then(|v| v.to_utf8().ok())
                    .is_some_and(|v| v.starts_with("zh"));
                Ok((paths, Arc::new(lease), chinese))
            })
            .await
            .map_err(|error| failure(error.to_string()))?;
            let workspace = NativeWorkspace::new(
                paths.clone(),
                generation,
                Arc::new(NativeOwnedShellCommandRunner),
                Some(
                    self.env
                        .iter()
                        .map(|(key, value)| (key.as_str().into(), value.as_str().into()))
                        .collect(),
                ),
            )
            .map_err(|error| failure(error.to_string()))?;
            let sink = Arc::new(PhaseEventSink::new(
                project.clone(),
                DiscoveryReportStore::new(paths.clone()),
                None,
                Arc::new(|message| eprintln!("{message}")),
            ));
            Ok(StandaloneProject {
                store: WorkCycleStore::new(paths),
                phase: ApplyPhase {
                    project_key: project.clone(),
                    gateway: self.gateway.clone(),
                    contexts: Arc::new(AlwaysOnRunContextRegistry::new()),
                    workspace: Arc::new(Workspace {
                        native: workspace,
                        signal: cancellation_signal(self.gateway.shutdown.clone()),
                    }),
                    events: sink.clone(),
                    observer: sink,
                    register_automation_tools: false,
                    chinese,
                },
            })
        })
    }
}
struct Workspace {
    native: NativeWorkspace,
    signal: AbortSignal,
}
impl ApplyWorkspace for Workspace {
    fn diff<'a>(
        &'a self,
        cycle: &'a WorkCycleRecord,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceDiff, ControlError>> + Send + 'a>> {
        Box::pin(async move {
            let result = async {
                let handle = self.native.authorize_cycle(cycle.clone()).await?;
                self.native
                    .diff(&handle.strategy, handle.cwd, Some(self.signal.clone()))
                    .await
            }
            .await;
            let closed = self.native.close().await;
            let value = result
                .and_then(|value| closed.map(|()| value))
                .map_err(|error| ControlError {
                    code: error.code,
                    message: String::from_utf16_lossy(error.message.units()),
                })?;
            Ok(WorkspaceDiff {
                text: value.text,
                file_count: value.file_count,
                truncated: value.truncated,
            })
        })
    }
}
fn cancellation_signal(signal: crate::runtime::network::Cancellation) -> AbortSignal {
    use crate::{
        host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
        runtime::tools::ToolRunContext,
    };
    let context = ToolRunContext::new(Arc::new(ToolRuntimeContext {
        fields: JsObject::new(),
        files: HostToolCheckpoint::default().tool_context_state(),
        abort_signal: Some(Arc::new(signal)),
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
    }));
    AbortSignal::from_context(&context).expect("native cancellation port is present")
}
