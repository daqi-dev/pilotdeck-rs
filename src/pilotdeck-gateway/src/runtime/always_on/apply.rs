//! Standalone apply phase. The host supplies bounded workspace inspection and
//! a turn facade bound to its existing session owner.
use std::{future::Future, pin::Pin, sync::Arc};

use pilotdeck_protocol::JsString;

use super::{
    fire::{ApplyPhaseInput, ApplyPhaseResult, derive_apply_session_key},
    manager::{
        AlwaysOnAgentGateway, AlwaysOnRunContext, AlwaysOnRunContextRegistry, ControlError,
        PhaseObserver, PhaseRequest,
    },
    phase_tools::PhaseHandlers,
};

pub(crate) struct WorkspaceDiff {
    pub text: JsString,
    pub file_count: usize,
    pub truncated: bool,
}

/// Implementations must bound filesystem traversal and the returned diff.
pub(crate) trait ApplyWorkspace: Send + Sync {
    fn diff<'a>(
        &'a self,
        cycle: &'a super::cycles::WorkCycleRecord,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceDiff, ControlError>> + Send + 'a>>;
}

/// The production sink persists the same project/run events as discovery.
pub(crate) trait ApplyEvents: Send + Sync {
    fn emit(&self, run_id: &str, phase: &str, error: Option<&ControlError>);
    fn drain<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

struct ContextGuard {
    registry: Arc<AlwaysOnRunContextRegistry>,
    session: String,
}
impl Drop for ContextGuard {
    fn drop(&mut self) {
        self.registry.unregister(&self.session);
    }
}

pub(crate) struct ApplyPhase {
    pub project_key: JsString,
    pub gateway: Arc<dyn AlwaysOnAgentGateway>,
    pub contexts: Arc<AlwaysOnRunContextRegistry>,
    pub workspace: Arc<dyn ApplyWorkspace>,
    pub events: Arc<dyn ApplyEvents>,
    pub observer: Arc<dyn PhaseObserver>,
    pub register_automation_tools: bool,
    pub chinese: bool,
}

impl ApplyPhase {
    pub async fn run(&self, input: ApplyPhaseInput<'_>) -> Result<ApplyPhaseResult, ControlError> {
        let Some(workspace) = input.cycle.workspace.as_ref().filter(|w| !w.cwd.is_empty()) else {
            return Ok(ApplyPhaseResult {
                session_key: String::new(),
                error: Some(ControlError {
                    code: "missing_workspace".into(),
                    message: "Cycle has no associated workspace to apply".into(),
                }),
            });
        };
        let workspace_cwd = workspace.cwd.to_utf8().map_err(|_| ControlError {
            code: "invalid_workspace".into(),
            message: "Workspace path cannot be represented by the native filesystem".into(),
        })?;
        let diff = self.workspace.diff(input.cycle).await?;
        let project_key = self.project_key.to_utf8().map_err(|_| ControlError {
            code: "invalid_project".into(),
            message: "Project path cannot be represented by the native filesystem".into(),
        })?;
        let session_key = derive_apply_session_key(&project_key, input.run_id);
        self.events.emit(input.run_id, "apply_started", None);
        let mut title = Vec::new();
        for (index, plan) in input.plans.iter().enumerate() {
            if index > 0 {
                title.extend("; ".encode_utf16());
            }
            if let Some(value) = &plan.title {
                title.extend_from_slice(value.units());
            }
        }
        let title = JsString::from_utf16(title);
        let branch = workspace
            .metadata
            .get_property("branchName")
            .and_then(|value| value.as_string());
        let missing_name = JsString::from("undefined");
        let prompt = apply_prompt(
            input.project_name.unwrap_or(&missing_name),
            input.project_root,
            &workspace_cwd,
            &workspace.strategy,
            &title,
            &input.cycle.id,
            branch,
            &diff,
            self.chinese,
        );
        let request = PhaseRequest {
            project_key: self.project_key.clone(),
            session_key: session_key.as_str().into(),
            channel_key: "always-on/apply".into(),
            run_id: format!("{}.apply", input.run_id).as_str().into(),
            message: prompt,
            cwd: input.project_root.into(),
            permission_mode: "bypassPermissions".into(),
            bypass_available: true,
            can_prompt: false,
            exclude_tools: crate::config::session_overrides::UNATTENDED_SESSION_EXCLUDED_TOOLS
                .into_iter()
                .map(JsString::from)
                .collect(),
            permission_deny: Vec::new(),
            context_registry: self.contexts.clone(),
            observer: self.observer.clone(),
            register_automation_tools: self.register_automation_tools,
        };
        let context = if self.register_automation_tools {
            self.contexts
                .register(AlwaysOnRunContext {
                    session_key: session_key.clone(),
                    run_id: input.run_id.into(),
                    project_key: project_key.clone(),
                })
                .map_err(|e| ControlError {
                    code: "internal".into(),
                    message: e.to_string(),
                })?;
            Some(ContextGuard {
                registry: self.contexts.clone(),
                session: session_key.clone(),
            })
        } else {
            None
        };
        let handlers = Arc::new(PhaseHandlers::Execution);
        let lease = if context.is_some() {
            Some(
                handlers
                    .install(&self.contexts, session_key.as_str().into())
                    .map_err(|e| ControlError {
                        code: "internal".into(),
                        message: e.to_string(),
                    })?,
            )
        } else {
            None
        };
        let response = self.gateway.run_phase(request).await;
        drop(lease);
        drop(context);
        handlers.drain().await;
        let mut result = response?;
        // The gateway has already supplied a phase default only for a
        // missing/null code. An explicitly empty code is a real TS value.
        let error = result.error.take();
        self.events.emit(
            input.run_id,
            if error.is_some() {
                "run_failed"
            } else {
                "apply_completed"
            },
            error.as_ref(),
        );
        self.events.drain().await;
        result.finish().await;
        Ok(ApplyPhaseResult { session_key, error })
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_prompt(
    project: &JsString,
    root: &str,
    cwd: &str,
    strategy: &str,
    title: &JsString,
    id: &JsString,
    branch: Option<&JsString>,
    diff: &WorkspaceDiff,
    chinese: bool,
) -> JsString {
    let mut units: Vec<u16> = if chinese {
        "为项目 ".encode_utf16().collect()
    } else {
        "Apply the isolated workspace changes to project "
            .encode_utf16()
            .collect()
    };
    units.extend_from_slice(project.units());
    let mut out = if chinese {
        format!(
            " 应用工作区变更。请直接合并已有改动，不要创建新计划。\n项目根目录：{root}\n隔离工作区：{cwd}（{strategy}）\n计划："
        )
    } else {
        format!(
            "; do not create a new plan.\nProject root: {root}\nWorkspace: {cwd} ({strategy})\nPlan: "
        )
    };
    units.extend(out.encode_utf16());
    units.extend_from_slice(title.units());
    units.extend(" (".encode_utf16());
    units.extend_from_slice(id.units());
    units.extend(")\n".encode_utf16());
    out.clear();
    if let Some(branch) = branch.filter(|branch| !branch.is_empty()) {
        units.extend(
            if chinese {
                "工作区分支："
            } else {
                "Branch: "
            }
            .encode_utf16(),
        );
        units.extend_from_slice(branch.units());
        units.push(10);
    }
    let empty = diff.text.to_utf8().is_ok_and(|text| text.trim().is_empty());
    if empty {
        out.push_str(if chinese {
            "没有检测到差异。"
        } else {
            "No changes were detected."
        });
    } else {
        if chinese {
            out.push_str(&format!(
                "\n变更内容：{} 个文件{}\n",
                diff.file_count,
                if diff.truncated {
                    "（已截断）"
                } else {
                    ""
                }
            ));
        } else {
            out.push_str(&format!(
                "\nFiles: {}{}\n",
                diff.file_count,
                if diff.truncated { " (truncated)" } else { "" }
            ));
        }
    }
    units.extend(out.encode_utf16());
    if !empty {
        units.extend_from_slice(diff.text.units());
    }
    JsString::from_utf16(units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::always_on::{
        cycles::{WorkCycleRecord, WorkCycleWorkspace},
        manager::{GatewayAbortInput, PhaseCompletion, PhaseResult},
    };
    use std::sync::Mutex;

    struct Gateway(
        Mutex<Vec<PhaseRequest>>,
        Option<ControlError>,
        Option<Arc<Events>>,
    );
    struct Completion(Arc<Events>);
    impl PhaseCompletion for Completion {
        fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move {
                self.0.0.lock().unwrap().push("closed".into());
            })
        }
    }
    impl AlwaysOnAgentGateway for Gateway {
        fn run_phase<'a>(
            &'a self,
            request: PhaseRequest,
        ) -> Pin<Box<dyn Future<Output = Result<PhaseResult, ControlError>> + Send + 'a>> {
            Box::pin(async move {
                if request.register_automation_tools {
                    assert!(
                        request
                            .context_registry
                            .get(&request.session_key.to_utf8().unwrap())
                            .is_some()
                    );
                }
                self.0.lock().unwrap().push(request);
                let mut terminal = pilotdeck_protocol::JsObject::new();
                terminal.insert(
                    "type".into(),
                    pilotdeck_protocol::JsValue::String("turn_completed".into()),
                );
                let result = PhaseResult::new(
                    vec![pilotdeck_protocol::JsValue::Object(terminal)],
                    self.1.clone(),
                );
                Ok(if let Some(events) = &self.2 {
                    result.with_completion(Box::new(Completion(events.clone())))
                } else {
                    result
                })
            })
        }
        fn abort_turn<'a>(
            &'a self,
            _: GatewayAbortInput<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }
    struct Workspace;
    impl ApplyWorkspace for Workspace {
        fn diff<'a>(
            &'a self,
            _: &'a WorkCycleRecord,
        ) -> Pin<Box<dyn Future<Output = Result<WorkspaceDiff, ControlError>> + Send + 'a>>
        {
            Box::pin(async {
                Ok(WorkspaceDiff {
                    text: "diff-content".into(),
                    file_count: 1,
                    truncated: false,
                })
            })
        }
    }
    #[derive(Default)]
    struct Events(Mutex<Vec<String>>);
    impl ApplyEvents for Events {
        fn emit(&self, _: &str, phase: &str, _: Option<&ControlError>) {
            self.0.lock().unwrap().push(phase.into());
        }
        fn drain<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            Box::pin(async {})
        }
    }
    impl PhaseObserver for Events {
        fn on_event<'a>(
            &'a self,
            _: &'a JsString,
            _: &'a JsString,
            _: &'a JsString,
            _: &'a pilotdeck_protocol::JsValue,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            Box::pin(async {})
        }
    }
    #[tokio::test]
    async fn applies_workspace_using_project_scoped_unattended_turn() {
        let events = Arc::new(Events::default());
        let gateway = Arc::new(Gateway(Mutex::default(), None, Some(events.clone())));
        let phase = ApplyPhase {
            project_key: "/project".into(),
            gateway: gateway.clone(),
            contexts: Arc::new(AlwaysOnRunContextRegistry::new()),
            workspace: Arc::new(Workspace),
            events: events.clone(),
            observer: events.clone(),
            register_automation_tools: false,
            chinese: false,
        };
        let cycle = WorkCycleRecord {
            raw: Default::default(),
            id: "cycle".into(),
            project_key: "/project".into(),
            status: "active".into(),
            workspace: Some(WorkCycleWorkspace {
                strategy: "git-worktree".into(),
                cwd: "/workspace".into(),
                metadata: Default::default(),
            }),
            plan_ids: Vec::new(),
            created_at: "".into(),
            created_by_run_id: "".into(),
        };
        let result = phase
            .run(ApplyPhaseInput {
                run_id: "run",
                cycle: &cycle,
                plans: &[],
                project_name: Some(&"Project".into()),
                project_root: "/project",
            })
            .await
            .unwrap();
        assert_eq!(
            result.session_key,
            "always-on/apply:project=/project:run=run"
        );
        assert!(result.error.is_none());
        let requests = gateway.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(request.project_key.equals_str("/project"));
        assert!(request.run_id.equals_str("run.apply"));
        assert_eq!(request.cwd, std::path::PathBuf::from("/project"));
        assert!(!request.can_prompt && request.bypass_available);
        assert!(!request.register_automation_tools);
        assert_eq!(request.exclude_tools.len(), 3);
        assert!(request.message.to_utf8().unwrap().contains("diff-content"));
        assert_eq!(
            *events.0.lock().unwrap(),
            ["apply_started", "apply_completed", "closed"]
        );
    }

    #[tokio::test]
    async fn enabled_apply_owns_context_until_turn_finishes() {
        let gateway = Arc::new(Gateway(Mutex::default(), None, None));
        let events = Arc::new(Events::default());
        let contexts = Arc::new(AlwaysOnRunContextRegistry::new());
        let phase = ApplyPhase {
            project_key: "/project".into(),
            gateway,
            contexts: contexts.clone(),
            workspace: Arc::new(Workspace),
            events: events.clone(),
            observer: events,
            register_automation_tools: true,
            chinese: false,
        };
        let cycle = WorkCycleRecord {
            raw: Default::default(),
            id: "cycle".into(),
            project_key: "/project".into(),
            status: "active".into(),
            workspace: Some(WorkCycleWorkspace {
                strategy: "git-worktree".into(),
                cwd: "/workspace".into(),
                metadata: Default::default(),
            }),
            plan_ids: Vec::new(),
            created_at: "".into(),
            created_by_run_id: "".into(),
        };
        let result = phase
            .run(ApplyPhaseInput {
                run_id: "run",
                cycle: &cycle,
                plans: &[],
                project_name: None,
                project_root: "/project",
            })
            .await
            .unwrap();
        assert!(result.error.is_none());
        assert!(contexts.get(&result.session_key).is_none());
    }

    #[tokio::test]
    async fn missing_workspace_never_submits_or_emits() {
        let gateway = Arc::new(Gateway(Mutex::default(), None, None));
        let events = Arc::new(Events::default());
        let phase = ApplyPhase {
            project_key: "/project".into(),
            gateway: gateway.clone(),
            contexts: Arc::new(AlwaysOnRunContextRegistry::new()),
            workspace: Arc::new(Workspace),
            events: events.clone(),
            observer: events.clone(),
            register_automation_tools: false,
            chinese: false,
        };
        let cycle = WorkCycleRecord {
            raw: Default::default(),
            id: "cycle".into(),
            project_key: "/project".into(),
            status: "active".into(),
            workspace: None,
            plan_ids: Vec::new(),
            created_at: "".into(),
            created_by_run_id: "".into(),
        };
        let result = phase
            .run(ApplyPhaseInput {
                run_id: "run",
                cycle: &cycle,
                plans: &[],
                project_name: None,
                project_root: "/project",
            })
            .await
            .unwrap();
        assert_eq!(result.error.unwrap().code, "missing_workspace");
        assert!(gateway.0.lock().unwrap().is_empty());
        assert!(events.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn turn_error_is_returned_and_reported() {
        let events = Arc::new(Events::default());
        let gateway = Arc::new(Gateway(
            Mutex::default(),
            Some(ControlError {
                code: String::new(),
                message: "turn failed".into(),
            }),
            Some(events.clone()),
        ));
        let phase = ApplyPhase {
            project_key: "/project".into(),
            gateway,
            contexts: Arc::new(AlwaysOnRunContextRegistry::new()),
            workspace: Arc::new(Workspace),
            events: events.clone(),
            observer: events.clone(),
            register_automation_tools: false,
            chinese: false,
        };
        let cycle = WorkCycleRecord {
            raw: Default::default(),
            id: "cycle".into(),
            project_key: "/project".into(),
            status: "active".into(),
            workspace: Some(WorkCycleWorkspace {
                strategy: "git-worktree".into(),
                cwd: "/workspace".into(),
                metadata: Default::default(),
            }),
            plan_ids: Vec::new(),
            created_at: "".into(),
            created_by_run_id: "".into(),
        };
        let result = phase
            .run(ApplyPhaseInput {
                run_id: "run",
                cycle: &cycle,
                plans: &[],
                project_name: None,
                project_root: "/project",
            })
            .await
            .unwrap();
        let error = result.error.unwrap();
        assert_eq!(error.code, "", "explicit empty code must not be replaced");
        assert_eq!(error.message, "turn failed");
        assert_eq!(
            *events.0.lock().unwrap(),
            ["apply_started", "run_failed", "closed"]
        );
    }

    #[test]
    fn chinese_apply_localizes_branch_and_diff_labels() {
        let diff = WorkspaceDiff {
            text: "+中文".into(),
            file_count: 2,
            truncated: true,
        };
        let prompt = apply_prompt(
            &"项目".into(),
            "/project",
            "/workspace",
            "git-worktree",
            &"计划".into(),
            &"id".into(),
            Some(&"topic".into()),
            &diff,
            true,
        );
        let text = prompt.to_utf8().expect("valid fixture");
        assert!(text.contains("工作区分支：topic"));
        assert!(text.contains("变更内容：2 个文件（已截断）"));
        assert!(text.contains("+中文"));
        assert!(!text.contains("Branch: "));
        assert!(!text.contains("Files: "));
    }

    #[test]
    fn truncated_diff_retains_lone_surrogate_in_model_message() {
        let diff = WorkspaceDiff {
            text: JsString::from_utf16(vec![b'x' as u16, 0xd83d]),
            file_count: 1,
            truncated: true,
        };
        let prompt = apply_prompt(
            &"p".into(),
            "/p",
            "/w",
            "git-worktree",
            &"title".into(),
            &"id".into(),
            None,
            &diff,
            false,
        );
        assert!(prompt.units().ends_with(&[b'x' as u16, 0xd83d]));
    }

    #[test]
    fn project_name_and_branch_keep_lone_surrogates() {
        let project = JsString::from_utf16(vec![0xd800]);
        let branch = JsString::from_utf16(vec![0xdc00]);
        let diff = WorkspaceDiff {
            text: "x".into(),
            file_count: 1,
            truncated: false,
        };
        let prompt = apply_prompt(
            &project,
            "/p",
            "/w",
            "git-worktree",
            &"title".into(),
            &"id".into(),
            Some(&branch),
            &diff,
            false,
        );
        assert!(prompt.units().contains(&0xd800));
        assert!(prompt.units().contains(&0xdc00));
    }
}
