//! The project-generation-owned Always-On discovery and execution pipeline.
use super::{
    AlwaysOnError, DiscoveryFirePort,
    apply::{ApplyPhase, ApplyWorkspace, WorkspaceDiff},
    chat_digest::{self, ChatDigest},
    chat_tool::{ChatHandler, NativeChatCatalog},
    event_sink::PhaseEventSink,
    fire::{
        ApplyPhaseInput, ApplyPhaseResult, FireOutcome, FireRunInput, RerunFireInput,
        RerunFireResult,
    },
    manager::{ControlError, FireInputs},
    phase_runner::{PhaseRunner, PhaseSpec},
    phase_tools::PhaseHandlers,
    plan_tool::{PlanHandler, PlanPending, PlanState},
    report_contract::{self, ReportMetadata},
    report_tool::{ReportHandler, ReportPending, ReportState},
    reports::DiscoveryReportStore,
    storage,
    workspace::NativeWorkspace,
    workspace_phase::WorkspacePhase,
};
use crate::runtime::{
    history::HistoryFiles,
    manager::{SessionCatalogListInput, SessionCatalogPort},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, io, path::PathBuf, pin::Pin, sync::Arc};

pub(crate) struct NativeFire {
    input: FireInputs,
    workspace: Arc<NativeWorkspace>,
    sink: Arc<PhaseEventSink>,
    history: HistoryFiles,
    catalog: Arc<NativeChatCatalog>,
    max_frame_bytes: usize,
    reports: DiscoveryReportStore,
}
struct WorkspaceAdapter(Arc<NativeWorkspace>);
impl ApplyWorkspace for WorkspaceAdapter {
    fn diff<'a>(
        &'a self,
        cycle: &'a super::WorkCycleRecord,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceDiff, ControlError>> + Send + 'a>> {
        Box::pin(async move {
            let handle = self
                .0
                .authorize_cycle(cycle.clone())
                .await
                .map_err(workspace_error)?;
            let diff = self
                .0
                .diff(&handle.strategy, handle.cwd, None)
                .await
                .map_err(workspace_error)?;
            Ok(WorkspaceDiff {
                text: diff.text,
                file_count: diff.file_count,
                truncated: diff.truncated,
            })
        })
    }
}
fn failure(code: &str, message: impl Into<String>) -> ControlError {
    ControlError {
        code: code.into(),
        message: message.into(),
    }
}
fn workspace_error(error: super::WorkspaceError) -> ControlError {
    failure(&error.code, String::from_utf16_lossy(error.message.units()))
}
fn js(s: &JsString) -> Result<String, ControlError> {
    s.to_utf8().map_err(|_| {
        failure(
            "invalid_input",
            "Identifier cannot be represented by this native operation",
        )
    })
}
fn to_io(error: ControlError) -> io::Error {
    io::Error::other(format!("{}: {}", error.code, error.message))
}
fn iso(ms: f64) -> JsString {
    crate::runtime::cron::iso_from_epoch_ms(ms).as_str().into()
}
impl NativeFire {
    pub(crate) fn build(
        input: FireInputs,
        workspace: Arc<NativeWorkspace>,
        sink: Arc<PhaseEventSink>,
        history: HistoryFiles,
        catalog: Arc<dyn SessionCatalogPort>,
        catalog_input: SessionCatalogListInput,
        max_frame_bytes: usize,
    ) -> io::Result<Arc<dyn DiscoveryFirePort>> {
        if max_frame_bytes <= 4096 {
            return Err(io::Error::other("Always-On frame budget is too small"));
        }
        let reports = DiscoveryReportStore::new(input.paths.clone());
        Ok(Arc::new(Self {
            input,
            workspace,
            sink,
            history,
            catalog: Arc::new(NativeChatCatalog {
                catalog,
                input: catalog_input,
            }),
            max_frame_bytes,
            reports,
        }))
    }
    fn phase_runner(&self) -> PhaseRunner {
        PhaseRunner {
            project: self.input.project_key.as_str().into(),
            gateway: self.input.gateway.clone(),
            contexts: self.input.contexts.clone(),
            observer: self.sink.clone(),
        }
    }
    fn workspace_phase(&self) -> WorkspacePhase {
        WorkspacePhase {
            project: self.input.project_key.as_str().into(),
            gateway: self.input.gateway.clone(),
            contexts: self.input.contexts.clone(),
            observer: self.sink.clone(),
            workspace: self.workspace.clone(),
            cycles: self.input.cycle_store.clone(),
            state: self.input.state_store.clone(),
            now: self.input.now.clone(),
            uuid: self.input.uuid.clone(),
            chinese: self.input.config.language.as_deref() == Some("zh-CN"),
        }
    }
    fn emit(
        &self,
        run: &str,
        phase: &str,
        plan: Option<&JsString>,
        title: Option<&JsString>,
        outcome: Option<&str>,
        error: Option<&ControlError>,
    ) {
        self.sink
            .emit_phase(run, phase, plan, title, outcome, error);
    }
    async fn complete(
        &self,
        run: &str,
        plan: Option<JsString>,
        outcome: &str,
        started: f64,
        workspace: Option<(JsString, &super::WorkspaceHandle)>,
        error: Option<&ControlError>,
    ) -> Result<(), ControlError> {
        let (cycle, workspace) = match workspace {
            Some((cycle, workspace)) => (Some(cycle), Some(workspace)),
            None => (None, None),
        };
        let now = (self.input.now)();
        let store = self.input.state_store.clone();
        let run_owned = run.to_owned();
        let status = outcome.to_owned();
        let plan_for_state = plan.clone();
        storage::run(move || {
            store.mark_fire_completed_js(&status, &run_owned, plan_for_state.as_ref(), now)?;
            if status == "no_plan" {
                store.set_dormant(now)?;
            }
            Ok(())
        })
        .await
        .map_err(|e| failure("internal", e.to_string()))?;
        let mut event = JsObject::new();
        event.insert("schemaVersion".into(), JsValue::Number(1.0));
        event.insert("runId".into(), run.into());
        event.insert("startedAt".into(), JsValue::String(iso(started)));
        event.insert("finishedAt".into(), JsValue::String(iso(now)));
        event.insert("outcome".into(), outcome.into());
        if let Some(plan) = plan {
            event.insert("planId".into(), JsValue::String(plan));
        }
        if let Some(cycle) = cycle {
            event.insert("workCycleId".into(), JsValue::String(cycle));
        }
        if let Some(workspace) = workspace {
            let mut value = JsObject::new();
            value.insert("strategy".into(), workspace.strategy.as_str().into());
            value.insert("handle".into(), JsValue::String(workspace.cwd.clone()));
            event.insert("workspace".into(), JsValue::Object(value));
        }
        if let Some(error) = error {
            let mut value = JsObject::new();
            value.insert("code".into(), error.code.as_str().into());
            value.insert("message".into(), error.message.as_str().into());
            event.insert("error".into(), JsValue::Object(value));
        }
        self.reports
            .append_history(event)
            .await
            .map_err(|e| failure("internal", e.to_string()))?;
        Ok(())
    }
    fn chinese(&self) -> bool {
        self.input.config.language.as_deref() == Some("zh-CN")
    }
    fn discovery_prompt(
        &self,
        digest: &ChatDigest,
        plans: &[super::DiscoveryPlanRecord],
    ) -> JsString {
        let intro = if self.chinese() {
            format!(
                "检查项目 {}，使用 always_on_discovery_plan 提出最多一个有用计划。请用简体中文撰写计划。\n",
                self.input.project_key
            )
        } else {
            format!(
                "Inspect project {} and propose at most one useful plan with always_on_discovery_plan.\n",
                self.input.project_key
            )
        };
        let mut out = intro.encode_utf16().collect::<Vec<_>>();
        for plan in plans {
            out.extend(
                if self.chinese() {
                    "已有计划："
                } else {
                    "Existing plan: "
                }
                .encode_utf16(),
            );
            out.extend_from_slice(plan.id.units());
            if let Some(title) = &plan.title {
                out.extend(" — ".encode_utf16());
                out.extend_from_slice(title.units());
            }
            out.push(10);
        }
        for session in &digest.sessions {
            out.extend(
                if self.chinese() {
                    "会话 "
                } else {
                    "Session "
                }
                .encode_utf16(),
            );
            out.extend_from_slice(session.alias.units());
            out.extend(" — ".encode_utf16());
            out.extend_from_slice(session.title.units());
            out.extend(" [".encode_utf16());
            out.extend_from_slice(session.modified.units());
            out.extend("]\n".encode_utf16());
            for prompt in &session.prompts {
                out.extend("  > ".encode_utf16());
                out.extend_from_slice(prompt.units());
                out.push(10);
            }
        }
        JsString::from_utf16(out)
    }
    async fn update_plan(
        &self,
        plan: &JsString,
        status: &str,
        cycle: Option<&JsString>,
        report: Option<&JsString>,
    ) -> Result<(), ControlError> {
        let store = self.input.plan_store.clone();
        let plan = plan.clone();
        let mut update = JsObject::new();
        update.insert("status".into(), status.into());
        if let Some(cycle) = cycle {
            update.insert("workCycleId".into(), JsValue::String(cycle.clone()));
        }
        if let Some(report) = report {
            update.insert("reportFilePath".into(), JsValue::String(report.clone()));
        }
        storage::run(move || store.update(&plan, &update).map(|_| ()))
            .await
            .map_err(|e| failure("internal", e.to_string()))
    }
    fn phase_spec(
        &self,
        run: &str,
        kind: &str,
        cwd: PathBuf,
        message: JsString,
        handlers: Arc<PhaseHandlers>,
    ) -> PhaseSpec {
        let project = &self.input.project_key;
        let session_key = match kind {
            "discovery" => super::fire::derive_discovery_session_key(project, run),
            "execute" => super::derive_execution_session_key(project, run),
            "report" => super::derive_report_session_key(project, run),
            _ => unreachable!(),
        };
        let deny = if kind == "execute" {
            ["git push*", "git remote*", "*git push*", "*git remote*"]
                .into_iter()
                .map(|pattern| {
                    crate::config::permission::PermissionRule::new("policy", "deny", "bash")
                        .with_pattern(pattern)
                })
                .collect()
        } else {
            Vec::new()
        };
        PhaseSpec {
            run_id: run.into(),
            session_key,
            channel_key: format!("always-on/{kind}").as_str().into(),
            phase_run: format!("{run}.{kind}").as_str().into(),
            message,
            cwd,
            deny,
            handlers,
        }
    }
    async fn write_report(
        &self,
        run: &str,
        plan: &JsString,
        started: f64,
        workspace: &super::WorkspaceHandle,
        content: JsString,
        outcome: &str,
    ) -> Result<JsString, ControlError> {
        let metadata = ReportMetadata {
            run_id: run.into(),
            plan_id: plan.clone(),
            started_at: iso(started),
            finished_at: iso((self.input.now)()),
            outcome: outcome.into(),
            workspace_strategy: workspace.strategy.as_str().into(),
            workspace_handle: workspace.cwd.clone(),
        };
        let report = report_contract::parse_report(&content, &metadata);
        let path = self
            .reports
            .write_report(run.into(), report.raw)
            .await
            .map_err(|e| failure("internal", e.to_string()))?;
        let path = path
            .to_str()
            .ok_or_else(|| failure("internal", "Report path is not UTF-8"))?;
        Ok(path.into())
    }
    async fn execute_plan(
        &self,
        run: &str,
        started: f64,
        state: &JsValue,
        plan: super::DiscoveryPlanRecord,
        markdown: JsString,
    ) -> Result<(FireOutcome, Option<ControlError>), ControlError> {
        let title = plan.title.clone().unwrap_or_else(|| plan.id.clone());
        self.emit(run, "workspace_started", Some(&plan.id), None, None, None);
        let ws = match self.workspace_phase().run(run, title.clone(), state).await {
            Ok(ws) => ws,
            Err(error) => {
                self.emit(
                    run,
                    "run_failed",
                    Some(&plan.id),
                    None,
                    Some("failed"),
                    Some(&error),
                );
                self.complete(run, Some(plan.id), "failed", started, None, Some(&error))
                    .await?;
                return Ok((
                    FireOutcome {
                        outcome: "failed".into(),
                    },
                    Some(error),
                ));
            }
        };
        self.emit(run, "workspace_ready", Some(&plan.id), None, None, None);
        self.update_plan(&plan.id, "executing", Some(&ws.cycle.id), None)
            .await?;
        let cycles = self.input.cycle_store.clone();
        let cycle = ws.cycle.id.clone();
        let id = plan.id.clone();
        storage::run(move || cycles.add_plan(&cycle, id))
            .await
            .map_err(|e| failure("internal", e.to_string()))?;
        let cwd = PathBuf::from(js(&ws.handle.cwd)?);
        let mut body = if self.chinese() { "在当前隔离工作区执行这个计划。禁止推送或更改远端。请用简体中文说明执行结果。\n\n" } else { "Execute this plan in the current isolated workspace. Do not push or change remotes.\n\n" }.encode_utf16().collect::<Vec<_>>();
        body.extend_from_slice(markdown.units());
        self.emit(
            run,
            "execution_started",
            Some(&plan.id),
            Some(&title),
            None,
            None,
        );
        let result = self
            .phase_runner()
            .run(self.phase_spec(
                run,
                "execute",
                cwd.clone(),
                JsString::from_utf16(body),
                Arc::new(PhaseHandlers::Execution),
            ))
            .await?;
        if let Some(error) = result.error {
            self.emit(
                run,
                "run_failed",
                Some(&plan.id),
                None,
                Some("failed"),
                Some(&error),
            );
            let content = format!(
                "# Work report\n\n## Notes\nExecution failed: {}",
                error.message
            );
            let report = self
                .write_report(
                    run,
                    &plan.id,
                    started,
                    &ws.handle,
                    content.as_str().into(),
                    "failed",
                )
                .await?;
            self.update_plan(&plan.id, "failed", Some(&ws.cycle.id), Some(&report))
                .await?;
            self.complete(
                run,
                Some(plan.id),
                "failed",
                started,
                Some((ws.cycle.id, &ws.handle)),
                Some(&error),
            )
            .await?;
            return Ok((
                FireOutcome {
                    outcome: "failed".into(),
                },
                Some(error),
            ));
        }
        self.emit(
            run,
            "execution_completed",
            Some(&plan.id),
            Some(&title),
            None,
            None,
        );
        self.emit(
            run,
            "report_started",
            Some(&plan.id),
            Some(&title),
            None,
            None,
        );
        let report_state = Arc::new(tokio::sync::Mutex::new(ReportState::default()));
        let handler = Arc::new(ReportHandler {
            run_id: run.into(),
            plan_id: plan.id.clone(),
            started_at: Some(iso(started)),
            workspace_strategy: ws.handle.strategy.as_str().into(),
            workspace_handle: ws.handle.cwd.clone(),
            store: self.reports.clone(),
            state: report_state.clone(),
            pending: Arc::<ReportPending>::default(),
        });
        let mut prompt = if self.chinese() {
            "用简体中文描述已完成的工作，并使用 always_on_report 保存报告。\n\n"
        } else {
            "Describe the completed work and use always_on_report to save the report.\n\n"
        }
        .encode_utf16()
        .collect::<Vec<_>>();
        prompt.extend_from_slice(markdown.units());
        let result = self
            .phase_runner()
            .run(self.phase_spec(
                run,
                "report",
                cwd,
                JsString::from_utf16(prompt),
                Arc::new(PhaseHandlers::Report(handler)),
            ))
            .await?;
        let mut report = report_state.lock().await.report.clone();
        if report.is_none() {
            let mut text = Vec::new();
            for event in &result.events {
                if event
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("assistant_text_delta"))
                    && let Some(value) = event.get("text").and_then(JsValue::as_string) {
                        text.extend_from_slice(value.units());
                    }
            }
            let whitespace = |c: &u16| matches!(*c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
            let first = text
                .iter()
                .position(|c| !whitespace(c))
                .unwrap_or(text.len());
            let last = text
                .iter()
                .rposition(|c| !whitespace(c))
                .map_or(first, |i| i + 1);
            if first != last {
                let path = self
                    .write_report(
                        run,
                        &plan.id,
                        started,
                        &ws.handle,
                        JsString::from_utf16(text[first..last].to_vec()),
                        "executed",
                    )
                    .await?;
                report = Some(super::report_tool::SavedReport {
                    markdown: JsString::default(),
                    file_path: path,
                    finished_at: iso((self.input.now)()),
                });
            }
        }
        let degraded = report.is_none() || result.error.is_some();
        let path = if let Some(report) = report {
            report.file_path
        } else {
            let reason = result
                .error
                .as_ref()
                .map_or("Report tool was not invoked", |e| e.message.as_str());
            let content = format!("# Work report\n\n## Notes\n{reason}");
            self.write_report(
                run,
                &plan.id,
                started,
                &ws.handle,
                content.as_str().into(),
                "executed",
            )
            .await?
        };
        if !degraded {
            self.emit(
                run,
                "report_produced",
                Some(&plan.id),
                Some(&title),
                Some("executed"),
                None,
            );
        }
        self.emit(
            run,
            "run_completed",
            Some(&plan.id),
            Some(&title),
            Some("executed"),
            None,
        );
        self.update_plan(
            &plan.id,
            if degraded {
                "completed_no_report"
            } else {
                "completed"
            },
            Some(&ws.cycle.id),
            Some(&path),
        )
        .await?;
        self.complete(
            run,
            Some(plan.id),
            "executed",
            started,
            Some((ws.cycle.id, &ws.handle)),
            result.error.as_ref(),
        )
        .await?;
        Ok((
            FireOutcome {
                outcome: "executed".into(),
            },
            result.error,
        ))
    }
}
impl DiscoveryFirePort for NativeFire {
    fn close<'a>(&'a self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.sink.drain().await;
            self.workspace.close().await.map_err(|e| {
                io::Error::other(format!(
                    "{}: {}",
                    e.code,
                    String::from_utf16_lossy(e.message.units())
                ))
            })?;
            if self.sink.failed_writes() != 0 {
                return Err(io::Error::other("Always-On event persistence failed"));
            }
            Ok(())
        })
    }
    fn run<'a>(
        &'a self,
        input: FireRunInput<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<FireOutcome, AlwaysOnError>> + Send + 'a>> {
        Box::pin(async move {
            let work = async {
                let state = JsValue::Object(
                    self.input
                        .state_store
                        .read_async(input.started_at_ms)
                        .await
                        .map_err(|e| failure("internal", e.to_string()))?,
                );
                self.emit(input.run_id, "discovery_started", None, None, None, None);
                let digest = chat_digest::build_digest(self.catalog.clone(), &self.history)
                    .await
                    .map_err(|e| {
                        failure("internal", String::from_utf16_lossy(e.message.units()))
                    })?;
                let store = self.input.plan_store.clone();
                let plans = storage::run(move || store.list_records())
                    .await
                    .map_err(|e| failure("internal", e.to_string()))?;
                let prompt = self.discovery_prompt(&digest, &plans);
                let plan_state = Arc::new(tokio::sync::Mutex::new(PlanState::default()));
                let plan_handler = Arc::new(PlanHandler {
                    run_id: input.run_id.into(),
                    store: (*self.input.plan_store).clone(),
                    state: plan_state.clone(),
                    pending: Arc::<PlanPending>::default(),
                });
                let chat_handler = Arc::new(ChatHandler {
                    run_id: input.run_id.into(),
                    files: self.history.clone(),
                    aliases: digest.aliases,
                    catalog: self.catalog.clone(),
                    max_frame_bytes: self.max_frame_bytes,
                });
                let mut cwd = PathBuf::from(&self.input.project_key);
                let mut has_workspace = false;
                if let Some(id) = state.get("activeWorkCycleId").and_then(JsValue::as_string) {
                    let cycles = self.input.cycle_store.clone();
                    let id = id.clone();
                    if let Some(cycle) = storage::run(move || cycles.get_record_js(&id))
                        .await
                        .map_err(|e| failure("internal", e.to_string()))?
                        && cycle.status == "active" {
                            match self.workspace.authorize_cycle(cycle).await {
                                Ok(handle) => {
                                    cwd = PathBuf::from(js(&handle.cwd)?);
                                    has_workspace = true;
                                }
                                Err(e) if e.is_missing() => {}
                                Err(e) => return Err(workspace_error(e)),
                            }
                        }
                }
                if !has_workspace
                    && let Some(legacy) = state
                        .get("currentWorkspace")
                        .and_then(JsValue::as_object)
                        .cloned()
                    {
                        match self.workspace.authorize_legacy(legacy).await {
                            Ok(handle) => cwd = PathBuf::from(js(&handle.cwd)?),
                            Err(e) if e.is_missing() => {}
                            Err(e) => return Err(workspace_error(e)),
                        }
                    }
                let spec = self.phase_spec(
                    input.run_id,
                    "discovery",
                    cwd,
                    prompt,
                    Arc::new(PhaseHandlers::Discovery {
                        plan: plan_handler,
                        chat: chat_handler,
                    }),
                );
                let result = self.phase_runner().run(spec).await?;
                let plan = plan_state.lock().await.plan.clone();
                if let Some((plan, markdown)) = plan {
                    self.emit(
                        input.run_id,
                        "plan_produced",
                        Some(&plan.id),
                        plan.title.as_ref(),
                        None,
                        None,
                    );
                    self.execute_plan(input.run_id, input.started_at_ms, &state, plan, markdown)
                        .await
                        .map(|(outcome, _)| outcome)
                } else if let Some(error) = result.error {
                    self.emit(
                        input.run_id,
                        "run_failed",
                        None,
                        None,
                        Some("failed"),
                        Some(&error),
                    );
                    self.complete(
                        input.run_id,
                        None,
                        "failed",
                        input.started_at_ms,
                        None,
                        Some(&error),
                    )
                    .await?;
                    Ok(FireOutcome {
                        outcome: "failed".into(),
                    })
                } else {
                    self.emit(input.run_id, "no_plan", None, None, Some("no_plan"), None);
                    self.complete(
                        input.run_id,
                        None,
                        "no_plan",
                        input.started_at_ms,
                        None,
                        None,
                    )
                    .await?;
                    Ok(FireOutcome {
                        outcome: "no_plan".into(),
                    })
                }
            }
            .await;
            self.sink.drain().await;
            work.map_err(|error: ControlError| {
                AlwaysOnError::new(error.code, error.message.as_str())
            })
        })
    }
    fn rerun_plan<'a>(
        &'a self,
        input: RerunFireInput<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<RerunFireResult>> + Send + 'a>> {
        Box::pin(async move {
            let id: JsString = input.plan_id.into();
            let store = self.input.plan_store.clone();
            let lookup = id.clone();
            let found = storage::run(move || {
                Ok((
                    store.get_record_js(&lookup)?,
                    store.read_plan_markdown_js(&lookup)?,
                ))
            })
            .await?;
            let (plan, markdown) = match found {
                (None, _) => {
                    return Ok(RerunFireResult {
                        run_id: input.run_id.into(),
                        error: Some(failure(
                            "plan_not_found",
                            format!("Plan {} not found", input.plan_id),
                        )),
                    });
                }
                (Some(_), None) => {
                    return Ok(RerunFireResult {
                        run_id: input.run_id.into(),
                        error: Some(failure(
                            "plan_body_missing",
                            format!("Plan markdown for {} not found on disk", input.plan_id),
                        )),
                    });
                }
                (Some(plan), Some(markdown)) => (plan, markdown),
            };
            self.update_plan(&id, "ready", None, None)
                .await
                .map_err(to_io)?;
            let state = JsValue::Object(
                self.input
                    .state_store
                    .read_async(input.started_at_ms)
                    .await?,
            );
            let result = self
                .execute_plan(
                    input.run_id,
                    input.started_at_ms,
                    &state,
                    plan,
                    markdown.as_str().into(),
                )
                .await;
            self.sink.drain().await;
            Ok(RerunFireResult {
                run_id: input.run_id.into(),
                error: match result {
                    Ok((_, error)) => error,
                    Err(error) => Some(error),
                },
            })
        })
    }
    fn run_apply_phase<'a>(
        &'a self,
        input: ApplyPhaseInput<'a>,
    ) -> Pin<Box<dyn Future<Output = io::Result<ApplyPhaseResult>> + Send + 'a>> {
        Box::pin(async move {
            let phase = ApplyPhase {
                project_key: self.input.project_key.as_str().into(),
                gateway: self.input.gateway.clone(),
                contexts: self.input.contexts.clone(),
                workspace: Arc::new(WorkspaceAdapter(self.workspace.clone())),
                events: self.sink.clone(),
                observer: self.sink.clone(),
                register_automation_tools: true,
                chinese: self.input.config.language.as_deref() == Some("zh-CN"),
            };
            phase.run(input).await.map_err(to_io)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        always_on::{
            AlwaysOnConfig, AlwaysOnRunContextRegistry, GatewayAbortInput, PhaseRequest,
            PhaseResult,
        },
        manager::ManagerError,
        project::{ProjectRuntimeRegistry, ResourceBundle},
        tools::builtin::bash::runner::NativeOwnedShellCommandRunner,
    };
    use std::path::Path;
    struct EmptyCatalog;
    impl SessionCatalogPort for EmptyCatalog {
        fn list(&self, _: &SessionCatalogListInput) -> Result<Vec<JsValue>, ManagerError> {
            Ok(Vec::new())
        }
    }
    struct NoPlan;
    impl super::super::AlwaysOnAgentGateway for NoPlan {
        fn abort_turn<'a>(
            &'a self,
            _: GatewayAbortInput<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
        fn run_phase<'a>(
            &'a self,
            request: PhaseRequest,
        ) -> Pin<Box<dyn Future<Output = Result<PhaseResult, ControlError>> + Send + 'a>> {
            Box::pin(async move {
                assert!(request.channel_key.equals_str("always-on/discovery"));
                assert!(
                    request
                        .context_registry
                        .get(&request.session_key.to_utf8().unwrap())
                        .is_some()
                );
                assert!(
                    request
                        .message
                        .to_utf8()
                        .unwrap()
                        .contains("always_on_discovery_plan")
                );
                Ok(PhaseResult::new(Vec::new(), None))
            })
        }
    }
    struct FullRun(std::sync::Mutex<Vec<String>>, bool);
    impl super::super::AlwaysOnAgentGateway for FullRun {
        fn abort_turn<'a>(
            &'a self,
            _: GatewayAbortInput<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
        fn run_phase<'a>(
            &'a self,
            request: PhaseRequest,
        ) -> Pin<Box<dyn Future<Output = Result<PhaseResult, ControlError>> + Send + 'a>> {
            Box::pin(async move {
                use crate::{
                    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
                    runtime::tools::{ToolRegistry, ToolRunContext},
                };
                let phase = request.channel_key.to_utf8().unwrap();
                self.0.lock().unwrap().push(phase.clone());
                let prompt = request.message.to_utf8().unwrap();
                let expected = match (phase.as_str(), self.1) {
                    ("always-on/discovery", true) => "检查项目",
                    ("always-on/discovery", false) => "Inspect project",
                    ("always-on/workspace", true) => "创建隔离工作区",
                    ("always-on/workspace", false) => "Prepare an isolated workspace",
                    ("always-on/execute", true) => "在当前隔离工作区",
                    ("always-on/execute", false) => "Execute this plan",
                    ("always-on/report", true) => "用简体中文描述",
                    ("always-on/report", false) => "Describe the completed work",
                    _ => panic!("unexpected phase"),
                };
                assert!(
                    prompt.contains(expected),
                    "actual {phase} request: {prompt}"
                );
                let registry = ToolRegistry::default();
                super::super::tools::register_tools(
                    &registry,
                    request.context_registry.clone(),
                    &request.session_key,
                )
                .map_err(|e| failure("tool_registry", e.to_string()))?;
                let (name, input) = match phase.as_str() {
                    "always-on/discovery" => {
                        let oracle = pilotdeck_protocol::parse_json(include_str!(
                            "../../../tests/fixtures/always-on-cli/plan-oracle.json"
                        ))
                        .unwrap();
                        let JsValue::Array(cases) = oracle.get("records").unwrap() else {
                            panic!("cases")
                        };
                        let mut data = JsObject::new();
                        for (key, value) in [
                            ("title", "Named"),
                            ("summary", "summary"),
                            ("rationale", "why"),
                            ("dedupeKey", "key"),
                        ] {
                            data.insert(key.into(), value.into());
                        }
                        data.insert("content".into(), cases[0].get("input").unwrap().clone());
                        (Some("always_on_discovery_plan"), JsValue::Object(data))
                    }
                    "always-on/workspace" => {
                        let mut data = JsObject::new();
                        data.insert("strategy".into(), "snapshot-copy".into());
                        (Some("always_on_prepare_workspace"), JsValue::Object(data))
                    }
                    "always-on/report" => {
                        let mut data = JsObject::new();
                        data.insert("content".into(), "# Result\n## Notes\ncompleted\n".into());
                        (Some("always_on_report"), JsValue::Object(data))
                    }
                    "always-on/execute" => {
                        assert_eq!(request.permission_deny.len(), 4);
                        (None, JsValue::Undefined)
                    }
                    _ => panic!("unexpected phase"),
                };
                if let Some(name) = name {
                    let tool = registry.get(&name.into()).expect("registered phase tool");
                    let host = ToolRuntimeContext {
                        fields: Default::default(),
                        files: HostToolCheckpoint::default().tool_context_state(),
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
                    };
                    tool.execute(input, ToolRunContext::new(Arc::new(host)))
                        .await
                        .map_err(|e| {
                            failure(&e.code, String::from_utf16_lossy(e.message.units()))
                        })?;
                }
                Ok(PhaseResult::new(Vec::new(), None))
            })
        }
    }
    #[tokio::test]
    async fn real_stores_and_phase_tools_complete_a_full_fire() {
        full_fire(false).await;
    }
    #[tokio::test]
    async fn chinese_language_reaches_every_actual_phase_request() {
        full_fire(true).await;
    }
    async fn full_fire(chinese: bool) {
        let root =
            std::env::temp_dir().join(format!("pdrs-native-fire-full-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let home = root.join("home");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(project.join("fixture.txt"), "initial").unwrap();
        let md = crate::metadata::Metadata {
            home: home.clone(),
            cwd: Default::default(),
            files: crate::metadata::NativeFiles,
        };
        let paths =
            super::super::resolve_always_on_paths(&home, project.to_str().unwrap(), &md).unwrap();
        let registry = Arc::new(ProjectRuntimeRegistry::new(
            project.clone(),
            Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(())),
        ));
        let lease = Arc::new(registry.acquire(None).unwrap());
        let env = Some(vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PILOTDECK_SHELL_PATH".into(), "/bin/sh".into()),
        ]);
        let workspace = Arc::new(
            NativeWorkspace::new(
                paths.clone(),
                lease,
                Arc::new(NativeOwnedShellCommandRunner),
                env,
            )
            .unwrap(),
        );
        let state = Arc::new(super::super::DiscoveryStateStore::new(paths.clone()));
        let plans = Arc::new(super::super::DiscoveryPlanStore::new(paths.clone()));
        let cycles = Arc::new(super::super::WorkCycleStore::new(paths.clone()));
        let sink = Arc::new(PhaseEventSink::new(
            project.to_str().unwrap().into(),
            DiscoveryReportStore::new(paths.clone()),
            None,
            Arc::new(|_| {}),
        ));
        let gateway = Arc::new(FullRun(std::sync::Mutex::new(Vec::new()), chinese));
        let mut config = AlwaysOnConfig::default_config();
        if chinese {
            config.language = Some("zh-CN".into());
        }
        let input = FireInputs {
            config,
            project_key: project.to_str().unwrap().into(),
            paths: paths.clone(),
            state_store: state.clone(),
            plan_store: plans.clone(),
            cycle_store: cycles.clone(),
            contexts: Arc::new(AlwaysOnRunContextRegistry::new()),
            gateway: gateway.clone(),
            now: Arc::new(|| 1_000.0),
            uuid: Arc::new(|| "cycle".into()),
        };
        let fire = NativeFire::build(
            input,
            workspace,
            sink,
            HistoryFiles {
                home: home.clone(),
                project: project.clone(),
            },
            Arc::new(EmptyCatalog),
            SessionCatalogListInput::new(&project, &home),
            16 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(
            fire.run(FireRunInput {
                run_id: "run",
                started_at_ms: 0.0
            })
            .await
            .unwrap()
            .outcome,
            "executed"
        );
        assert_eq!(
            *gateway.0.lock().unwrap(),
            [
                "always-on/discovery",
                "always-on/workspace",
                "always-on/execute",
                "always-on/report"
            ]
        );
        let plan = plans.get_record("p1").unwrap().unwrap();
        assert!(
            plan.raw
                .get("status")
                .and_then(JsValue::as_string)
                .unwrap()
                .equals_str("completed")
        );
        assert!(
            plan.raw
                .get("reportFilePath")
                .and_then(JsValue::as_string)
                .is_some()
        );
        assert!(
            cycles
                .get_record("cycle")
                .unwrap()
                .unwrap()
                .plan_ids
                .iter()
                .any(|id| id.equals_str("p1"))
        );
        let events = std::fs::read_to_string(paths.events_file()).unwrap();
        for phase in [
            "discovery_started",
            "plan_produced",
            "workspace_ready",
            "execution_completed",
            "report_produced",
            "run_completed",
        ] {
            assert!(events.contains(phase));
        }
        fire.close().await.unwrap();
        drop(fire);
        registry.dispose().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn real_no_plan_persists_dormancy_and_closes_project_generation() {
        let root = std::env::temp_dir().join(format!("pdrs-native-fire-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let home = root.join("home");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let metadata = crate::metadata::Metadata {
            home: home.clone(),
            cwd: Default::default(),
            files: crate::metadata::NativeFiles,
        };
        let paths =
            super::super::resolve_always_on_paths(&home, project.to_str().unwrap(), &metadata)
                .unwrap();
        let registry = Arc::new(ProjectRuntimeRegistry::new(
            project.clone(),
            Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(())),
        ));
        let lease = Arc::new(registry.acquire(None).unwrap());
        let workspace = Arc::new(
            NativeWorkspace::new(
                paths.clone(),
                lease,
                Arc::new(NativeOwnedShellCommandRunner),
                None,
            )
            .unwrap(),
        );
        let state = Arc::new(super::super::DiscoveryStateStore::new(paths.clone()));
        let sink = Arc::new(PhaseEventSink::new(
            project.to_str().unwrap().into(),
            DiscoveryReportStore::new(paths.clone()),
            None,
            Arc::new(|_| {}),
        ));
        let input = FireInputs {
            config: AlwaysOnConfig::default_config(),
            project_key: project.to_str().unwrap().into(),
            paths: paths.clone(),
            state_store: state.clone(),
            plan_store: Arc::new(super::super::DiscoveryPlanStore::new(paths.clone())),
            cycle_store: Arc::new(super::super::WorkCycleStore::new(paths.clone())),
            contexts: Arc::new(AlwaysOnRunContextRegistry::new()),
            gateway: Arc::new(NoPlan),
            now: Arc::new(|| 1_000.0),
            uuid: Arc::new(|| "cycle".into()),
        };
        let fire = NativeFire::build(
            input,
            workspace,
            sink,
            HistoryFiles {
                home: home.clone(),
                project: project.clone(),
            },
            Arc::new(EmptyCatalog),
            SessionCatalogListInput::new(&project, &home),
            16 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(
            fire.run(FireRunInput {
                run_id: "run",
                started_at_ms: 0.0
            })
            .await
            .unwrap()
            .outcome,
            "no_plan"
        );
        let value = state.read_async(1_000.0).await.unwrap();
        assert!(
            value
                .get("lastFireOutcome")
                .and_then(JsValue::as_string)
                .unwrap()
                .equals_str("no_plan")
        );
        assert!(value.get("dormant").and_then(JsValue::as_object).is_some());
        let events = std::fs::read_to_string(paths.events_file()).unwrap();
        assert!(events.contains("discovery_started") && events.contains("no_plan"));
        fire.close().await.unwrap();
        drop(fire);
        registry.dispose().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
