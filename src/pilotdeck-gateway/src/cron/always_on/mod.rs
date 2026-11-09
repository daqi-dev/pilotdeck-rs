//! Native Always-On controls. The application supplies the native agent turn facade.
mod chat;
mod plan;
mod prompts;
pub mod report;
mod scheduler;
pub mod storage;
#[cfg(test)]
mod tests;
mod tools;
pub mod workspace;

use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
pub use tools::RunTools;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlwaysOnError {
    pub code: String,
    pub message: String,
}
impl AlwaysOnError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn value(&self) -> Value {
        json!({"code":self.code,"message":self.message})
    }
}
impl std::fmt::Display for AlwaysOnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AlwaysOnError {}
impl From<std::io::Error> for AlwaysOnError {
    fn from(error: std::io::Error) -> Self {
        Self::new("storage_failed", error.to_string())
    }
}
impl From<serde_json::Error> for AlwaysOnError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("storage_corrupt", error.to_string())
    }
}

pub type EventSink = Arc<dyn Fn(&Value) -> Result<(), AlwaysOnError> + Send + Sync>;

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AlwaysOnError>> + Send + 'a>>;
#[derive(Clone)]
pub struct TurnRequest {
    pub project_key: String,
    pub session_key: String,
    pub run_id: String,
    pub channel_key: String,
    pub message: String,
    pub cwd: PathBuf,
    pub permission_mode: &'static str,
    pub can_prompt: bool,
    pub excluded_tools: Vec<&'static str>,
    pub permission_rules: Value,
    pub tools: Arc<RunTools>,
    /// The native adapter invokes this once per streamed event, before returning.
    pub on_event: EventSink,
}

/// Implement using native submit/abort/close. No TS transport is part of this port.
pub trait AlwaysOnHost: Send + Sync {
    fn turn(&self, request: TurnRequest) -> HostFuture<'_, Vec<Value>>;
    fn abort<'a>(
        &'a self,
        session_key: &'a str,
        run_id: &'a str,
        reason: &'a str,
    ) -> HostFuture<'a, ()>;
    fn close<'a>(&'a self, session_key: &'a str) -> HostFuture<'a, ()>;
    fn on_turn_event(&self, session_key: &str, channel_key: &str, event: &Value);
    fn on_worktree_created(&self, _run_id: &str, _cwd: &Path) {}
    fn on_worktree_removed(&self, _cwd: &Path) {}
}

struct ActiveRun {
    project: String,
    sessions: Vec<String>,
}
#[derive(Default)]
struct ActiveState {
    stopping: bool,
    runs: HashMap<String, ActiveRun>,
}
struct Admission<'a> {
    service: &'a AlwaysOnService,
    id: String,
}
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.service
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runs
            .remove(&self.id);
        self.service.drained.notify_waiters();
    }
}

pub struct AlwaysOnService {
    home: PathBuf,
    config: Value,
    host: Arc<dyn AlwaysOnHost>,
    stores: Mutex<HashMap<String, storage::Store>>,
    active: Mutex<ActiveState>,
    drained: Notify,
    project_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    automation: scheduler::Automation,
}

pub fn now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;
    crate::runtime::background::iso(ms)
        .to_utf8()
        .unwrap_or_default()
}

pub fn session_key(project: &str, run: &str, phase: &str) -> String {
    format!("always-on/{phase}:project={project}:run={run}")
}

fn required<'a>(params: &'a Value, field: &str) -> Result<&'a str, AlwaysOnError> {
    params
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AlwaysOnError::new(
                "invalid_params",
                format!("{field} must be a non-empty string"),
            )
        })
}

impl AlwaysOnService {
    pub fn new(home: PathBuf, config: Value, host: Arc<dyn AlwaysOnHost>) -> Self {
        Self {
            home,
            config,
            host,
            stores: Mutex::new(HashMap::new()),
            active: Mutex::new(ActiveState::default()),
            drained: Notify::new(),
            project_locks: Mutex::new(HashMap::new()),
            automation: scheduler::Automation::default(),
        }
    }

    fn zh(&self) -> bool {
        self.config["language"] == "zh-CN"
    }

    fn store(&self, project: &str) -> Result<storage::Store, AlwaysOnError> {
        let mut stores = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = stores.get(project) {
            return Ok(store.clone());
        }
        let store = storage::Store::new(&self.home, Path::new(project))?;
        stores.insert(project.into(), store.clone());
        Ok(store)
    }

    fn enabled(&self, project: &str) -> bool {
        self.config["projects"][project]["enabled"] == true
    }
    fn manager_enabled(&self) -> bool {
        self.config["projects"]
            .as_object()
            .is_some_and(|p| p.values().any(|p| p["enabled"] == true))
    }

    fn admit(
        &self,
        project: &str,
        run: &str,
        phases: &[&str],
    ) -> Result<Admission<'_>, AlwaysOnError> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active.stopping {
            return Err(AlwaysOnError::new(
                "not_ready",
                "AlwaysOnRuntime.bindGateway not called",
            ));
        }
        active.runs.insert(
            run.into(),
            ActiveRun {
                project: project.into(),
                sessions: phases
                    .iter()
                    .map(|phase| session_key(project, run, phase))
                    .collect(),
            },
        );
        Ok(Admission {
            service: self,
            id: run.into(),
        })
    }

    pub async fn stop(&self) {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping = true;
        self.stop_schedulers().await;
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .runs
                .is_empty()
            {
                break;
            }
            notified.await;
        }
    }

    pub async fn handle(&self, method: &str, params: &Value) -> Result<Value, AlwaysOnError> {
        let project = required(params, "projectKey")?;
        match method {
            "always_on_apply" => {
                let id = required(params, "workCycleId")?;
                let name = required(params, "projectName")?;
                let result = self.apply(project, id, name).await;
                Ok(match result {
                    Ok((session, None)) => json!({"sessionKey":session}),
                    Ok((session, Some(error))) => {
                        json!({"sessionKey":session,"error":error.value()})
                    }
                    Err(error) => json!({"sessionKey":"","error":error.value()}),
                })
            }
            "always_on_rerun_plan" => {
                let plan = required(params, "planId")?;
                if let Some(error) = self.runtime_error(project, false) {
                    return Ok(json!({"runId":"","error":error.value()}));
                }
                let run = uuid::Uuid::new_v4().to_string();
                let _admission =
                    match self.admit(project, &run, &["workspace", "execute", "report"]) {
                        Ok(a) => a,
                        Err(e) => return Ok(json!({"runId":"","error":e.value()})),
                    };
                let lock = self
                    .project_locks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(project.into())
                    .or_default()
                    .clone();
                let _lock = lock.lock().await;
                Ok(match self.rerun(project, plan, &run, None).await {
                    Ok(None) => json!({"runId":run}),
                    Ok(Some(e)) | Err(e) => json!({"runId":run,"error":e.value()}),
                })
            }
            "always_on_abort" => {
                let session = required(params, "sessionKey")?;
                let reason = params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("always-on/abort_requested");
                if let Some(error) = self.runtime_error(project, true) {
                    return Ok(json!({"aborted":false,"sessionKey":session,"error":error.value()}));
                }
                let run = self
                    .active
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .runs
                    .iter()
                    .find(|(_, active)| {
                        active.project == project
                            && active.sessions.iter().any(|key| key == session)
                    })
                    .map(|(id, _)| id.clone());
                let Some(run) = run else {
                    return Ok(
                        json!({"aborted":false,"sessionKey":session,"error":{"code":"session_not_active","message":"No active Always-On run owns this session."}}),
                    );
                };
                let _phase = session
                    .strip_prefix("always-on/")
                    .and_then(|s| s.split_once(":project="))
                    .map(|(phase, _)| phase)
                    .unwrap_or("");
                Ok(match self.host.abort(session, &run, reason).await {
                    Ok(()) => json!({"aborted":true,"sessionKey":session,"runId":run}),
                    Err(e) => {
                        json!({"aborted":false,"sessionKey":session,"runId":run,"error":{"code":"abort_failed","message":e.message}})
                    }
                })
            }
            _ => Err(AlwaysOnError::new("unknown_method", method)),
        }
    }

    fn runtime_error(&self, project: &str, abort: bool) -> Option<AlwaysOnError> {
        if self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping
        {
            return Some(AlwaysOnError::new(
                "not_ready",
                "AlwaysOnRuntime.bindGateway not called",
            ));
        }
        if !self.manager_enabled() {
            return Some(AlwaysOnError::new(
                "not_configured",
                if abort {
                    "Always-On abort requires an enabled Always-On runtime."
                } else {
                    "Always-On plan rerun requires an enabled Always-On runtime."
                },
            ));
        }
        if !self.enabled(project) {
            return Some(AlwaysOnError::new(
                "project_not_found",
                format!("No Always-On runtime for project {project}"),
            ));
        }
        None
    }

    async fn phase(
        &self,
        store: &storage::Store,
        run: &str,
        phase: &str,
        cwd: &Path,
        message: String,
        tools: Arc<RunTools>,
    ) -> Result<Vec<Value>, AlwaysOnError> {
        let project = store.project.to_string_lossy();
        let session = session_key(&project, run, phase);
        let channel = format!("always-on/{phase}");
        let rules = if phase == "execute" {
            json!({"deny":[
            {"source":"policy","behavior":"deny","toolName":"bash","pattern":"git push*"},
            {"source":"policy","behavior":"deny","toolName":"bash","pattern":"git remote*"},
            {"source":"policy","behavior":"deny","toolName":"bash","pattern":"*git push*"},
            {"source":"policy","behavior":"deny","toolName":"bash","pattern":"*git remote*"}]})
        } else {
            json!({})
        };
        let event_store = store.clone();
        let event_host = self.host.clone();
        let event_session = session.clone();
        let event_channel = channel.clone();
        let event_log = matches!(phase, "execute" | "report" | "apply").then(|| {
            format!(
                "runs/{}.events.jsonl",
                storage::safe_id(&format!("{run}.{phase}"))
            )
        });
        let on_event: EventSink = Arc::new(move |event| {
            if let Some(path) = &event_log {
                event_store.append(path, event)?;
            }
            event_host.on_turn_event(&event_session, &event_channel, event);
            Ok(())
        });
        let result = self
            .host
            .turn(TurnRequest {
                project_key: project.into_owned(),
                session_key: session.clone(),
                run_id: format!("{run}.{phase}"),
                channel_key: channel.clone(),
                message,
                cwd: cwd.into(),
                permission_mode: "bypassPermissions",
                can_prompt: false,
                excluded_tools: vec!["enter_plan_mode", "exit_plan_mode", "ask_user_question"],
                permission_rules: rules,
                tools,
                on_event,
            })
            .await;
        // Close and clear host overrides on both transport failure and normal completion.
        let _ = self.host.close(&session).await;
        result
    }

    fn event(
        &self,
        store: &storage::Store,
        run: &str,
        phase: &str,
        extra: Value,
    ) -> Result<(), AlwaysOnError> {
        let mut event = json!({"schemaVersion":1,"eventId":uuid::Uuid::new_v4().to_string(),"runId":run,"projectKey":store.project,"phase":phase,"timestamp":now()});
        for (key, value) in extra.as_object().into_iter().flatten() {
            event[key] = value.clone();
        }
        store.append("events.jsonl", &event)
    }

    async fn apply(
        &self,
        project: &str,
        id: &str,
        name: &str,
    ) -> Result<(String, Option<AlwaysOnError>), AlwaysOnError> {
        if self.manager_enabled() && !self.enabled(project) {
            return Err(AlwaysOnError::new(
                "project_not_found",
                format!("No Always-On runtime for project {project}"),
            ));
        }
        let run = uuid::Uuid::new_v4().to_string();
        let _admission = self.admit(project, &run, &["apply"])?;
        let store = self.store(project)?;
        let cycle = store.record("cycles", id)?.ok_or_else(|| {
            AlwaysOnError::new("cycle_not_found", format!("Work cycle {id} not found"))
        })?;
        if cycle["workspace"]["cwd"].as_str().is_none_or(str::is_empty) {
            return Err(AlwaysOnError::new(
                "missing_workspace",
                "Cycle has no associated workspace to apply",
            ));
        }
        let diff = workspace::diff(&store, &cycle["workspace"]).await?;
        let plans = store.index("plans")?;
        let titles = plans["plans"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| {
                cycle["planIds"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&p["id"]))
            })
            .filter_map(|p| p["title"].as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let prompt = prompts::apply(
            self.zh(),
            project,
            name,
            id,
            &titles,
            &cycle["workspace"],
            &diff,
        );
        self.event(&store, &run, "apply_started", json!({"outcome":"executed"}))?;
        let tools = Arc::new(RunTools::new(
            store.clone(),
            &run,
            "apply",
            Value::Null,
            cycle["workspace"].clone(),
            now(),
        ));
        let result = self
            .phase(&store, &run, "apply", Path::new(project), prompt, tools)
            .await;
        let error = match result {
            Ok(events) => first_error(&events, "apply_failed"),
            Err(e) => Some(e),
        };
        let session = session_key(project, &run, "apply");
        if let Some(e) = error {
            self.event(
                &store,
                &run,
                "run_failed",
                json!({"outcome":"failed","error":e.value()}),
            )?;
            // Preserve the admitted session key when the turn itself fails.
            return Ok((session, Some(e)));
        }
        self.event(
            &store,
            &run,
            "apply_completed",
            json!({"outcome":"executed"}),
        )?;
        Ok((session, None))
    }

    async fn rerun(
        &self,
        project: &str,
        plan_id: &str,
        run: &str,
        started: Option<&str>,
    ) -> Result<Option<AlwaysOnError>, AlwaysOnError> {
        let store = self.store(project)?;
        let started = started.map(str::to_owned).unwrap_or_else(now);
        let plan = store.record("plans", plan_id)?.ok_or_else(|| {
            AlwaysOnError::new("plan_not_found", format!("Plan {plan_id} not found"))
        })?;
        let body = store
            .plan_body(plan_id)?
            .filter(|body| !body.is_empty())
            .ok_or_else(|| {
                AlwaysOnError::new(
                    "plan_body_missing",
                    format!("Plan markdown for {plan_id} not found on disk"),
                )
            })?;
        store.update_record("plans", plan_id, json!({"status":"ready"}))?;
        self.event(&store, run, "workspace_started", json!({"planId":plan_id}))?;
        let cycle = match self.ensure_cycle(&store, run, &plan).await {
            Ok(cycle) => cycle,
            Err(error) => {
                self.event(
                    &store,
                    run,
                    "run_failed",
                    json!({"planId":plan_id,"outcome":"failed","error":error.value()}),
                )?;
                store.complete(&now(), run, plan_id, "failed")?;
                store.append("run-history.jsonl", &json!({"schemaVersion":1,"runId":run,"planId":plan_id,"startedAt":started,"finishedAt":now(),"outcome":"failed","error":error.value()}))?;
                return Ok(Some(error));
            }
        };
        let mut workspace = cycle["workspace"].clone();
        workspace["metadata"]["startedAt"] = json!(started);
        let cwd = PathBuf::from(required(&workspace, "cwd")?);
        let cycle_id = required(&cycle, "id")?;
        self.event(&store, run, "workspace_ready", json!({"planId":plan_id}))?;
        store.update_record(
            "plans",
            plan_id,
            json!({"status":"executing","workCycleId":cycle_id}),
        )?;
        store.add_plan(cycle_id, plan_id)?;
        self.event(
            &store,
            run,
            "execution_started",
            json!({"planId":plan_id,"title":plan["title"]}),
        )?;
        let tools = Arc::new(RunTools::new(
            store.clone(),
            run,
            "execute",
            plan.clone(),
            workspace.clone(),
            started.clone(),
        ));
        let prompt = prompts::execute(self.zh(), &workspace, &body);
        let execution = self
            .phase(&store, run, "execute", &cwd, prompt, tools)
            .await;
        let execution_error = match execution {
            Ok(events) => first_error(&events, "execution_failed"),
            Err(error) => Some(error),
        };
        if let Some(error) = execution_error {
            self.event(
                &store,
                run,
                "run_failed",
                json!({"planId":plan_id,"outcome":"failed","error":error.value()}),
            )?;
            let finished = now();
            let metadata = report_metadata(run, plan_id, &started, &finished, "failed", &workspace);
            let report = store.report(
                run,
                &report::fallback(
                    plan["title"].as_str().unwrap_or(""),
                    &format!("execution_failed: {}", error.message),
                    &metadata,
                ),
            )?;
            store.update_record(
                "plans",
                plan_id,
                json!({"status":"failed","reportFilePath":report,"workCycleId":cycle_id}),
            )?;
            self.finish(
                &store,
                run,
                plan_id,
                &started,
                &finished,
                "failed",
                &cycle,
                Some(&error),
            )?;
            return Ok(Some(error));
        }
        self.event(
            &store,
            run,
            "execution_completed",
            json!({"planId":plan_id,"title":plan["title"]}),
        )?;
        self.event(
            &store,
            run,
            "report_started",
            json!({"planId":plan_id,"title":plan["title"]}),
        )?;
        let tools = Arc::new(RunTools::new(
            store.clone(),
            run,
            "report",
            plan.clone(),
            workspace.clone(),
            started.clone(),
        ));
        let prompt = prompts::report(self.zh(), &workspace, &body);
        let result = self
            .phase(&store, run, "report", &cwd, prompt, tools.clone())
            .await;
        let (events, error) = match result {
            Ok(events) => {
                let error = first_error(&events, "report_degraded");
                (events, error)
            }
            Err(error) => (vec![], Some(error)),
        };
        let finished = now();
        let mut report = tools.report_path();
        if report.is_none() {
            let text: String = events
                .iter()
                .filter(|e| e["type"] == "assistant_text_delta")
                .filter_map(|e| e["text"].as_str())
                .collect();
            if !text.trim().is_empty() {
                let metadata =
                    report_metadata(run, plan_id, &started, &finished, "executed", &workspace);
                report = Some(store.report(run, &report::normalize(&text, &metadata).0)?);
            }
        }
        let degraded = report.is_none() || error.is_some();
        if !degraded {
            self.event(
                &store,
                run,
                "report_produced",
                json!({"planId":plan_id,"title":plan["title"],"outcome":"executed"}),
            )?;
        }
        self.event(
            &store,
            run,
            "run_completed",
            json!({"planId":plan_id,"title":plan["title"],"outcome":"executed"}),
        )?;
        let report = match report {
            Some(path) => path,
            None => {
                let metadata =
                    report_metadata(run, plan_id, &started, &finished, "failed", &workspace);
                let reason = error.as_ref().map_or_else(
                    || "report_tool_not_invoked".into(),
                    |e| format!("report_failed: {}", e.message),
                );
                store.report(
                    run,
                    &report::fallback(plan["title"].as_str().unwrap_or(""), &reason, &metadata),
                )?
            }
        };
        store.update_record("plans", plan_id, json!({"status":if degraded { "completed_no_report" } else { "completed" },"reportFilePath":report,"workCycleId":cycle_id}))?;
        self.finish(
            &store,
            run,
            plan_id,
            &started,
            &finished,
            "executed",
            &cycle,
            error.as_ref(),
        )?;
        Ok(error)
    }

    async fn ensure_cycle(
        &self,
        store: &storage::Store,
        run: &str,
        plan: &Value,
    ) -> Result<Value, AlwaysOnError> {
        let state = store.state(&now())?;
        if let Some(id) = state["activeWorkCycleId"].as_str()
            && let Some(cycle) = store.record("cycles", id)?
            && cycle["status"] == "active"
            && cycle["workspace"]["cwd"]
                .as_str()
                .is_some_and(|cwd| Path::new(cwd).exists())
        {
            workspace::validate(store, &cycle["workspace"])?;
            return Ok(cycle);
        }
        let reuse_workspace = state["currentWorkspace"]["cwd"]
            .as_str()
            .is_some_and(|cwd| Path::new(cwd).exists());
        let workspace = if reuse_workspace {
            let reference = &state["currentWorkspace"];
            let workspace = json!({"strategy":reference["strategy"],"cwd":reference["cwd"],"metadata":reference["metadata"]});
            workspace::validate(store, &workspace)?;
            workspace
        } else {
            let tools = Arc::new(
                RunTools::new(
                    store.clone(),
                    run,
                    "workspace",
                    plan.clone(),
                    Value::Null,
                    now(),
                )
                .with_workspace_host(self.host.clone()),
            );
            let prompt = prompts::workspace(
                self.zh(),
                &store.project.to_string_lossy(),
                plan["title"].as_str().unwrap_or(""),
            );
            // Baseline falls back to deterministic preparation if the agent omitted the workspace tool.
            let _ = self
                .phase(
                    store,
                    run,
                    "workspace",
                    &store.project,
                    prompt,
                    tools.clone(),
                )
                .await?;
            let prepared = tools.prepared_workspace();
            match prepared {
                Some(workspace) => workspace,
                None => {
                    workspace::prepare_notified(
                        store,
                        run,
                        plan["title"].as_str().unwrap_or(""),
                        "auto",
                        Some(self.host.as_ref()),
                    )
                    .await?
                }
            }
        };
        let cycle_id = uuid::Uuid::new_v4().to_string();
        let cycle = json!({"id":cycle_id,"projectKey":store.project,"status":"active","workspace":workspace,"planIds":[],"createdAt":now(),"createdByRunId":state["currentWorkspace"]["runId"].as_str().unwrap_or(run)});
        store.insert_record("cycles", cycle.clone())?;
        store.update_state(
            &now(),
            json!({"activeWorkCycleId":cycle_id,"currentWorkspace":null}),
        )?;
        Ok(cycle)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        store: &storage::Store,
        run: &str,
        plan: &str,
        started: &str,
        finished: &str,
        outcome: &str,
        cycle: &Value,
        error: Option<&AlwaysOnError>,
    ) -> Result<(), AlwaysOnError> {
        store.complete(finished, run, plan, outcome)?;
        let mut history = json!({"schemaVersion":1,"runId":run,"planId":plan,"startedAt":started,"finishedAt":finished,"outcome":outcome,"workCycleId":cycle["id"],"workspace":{"strategy":cycle["workspace"]["strategy"],"handle":cycle["workspace"]["cwd"]}});
        if let Some(error) = error {
            history["error"] = error.value();
        }
        store.append("run-history.jsonl", &history)
    }
}

pub fn first_error(events: &[Value], fallback: &str) -> Option<AlwaysOnError> {
    if events
        .iter()
        .any(|e| e["type"] == "turn_completed" && e["finishReason"] == "aborted_streaming")
    {
        return Some(AlwaysOnError::new(
            "agent_aborted",
            "Agent turn was aborted.",
        ));
    }
    events.iter().find(|e| e["type"] == "error").map(|e| {
        AlwaysOnError::new(
            e["code"].as_str().unwrap_or(fallback),
            e["message"].as_str().unwrap_or("Agent turn failed"),
        )
    })
}

pub fn report_metadata(
    run: &str,
    plan: &str,
    started: &str,
    finished: &str,
    outcome: &str,
    workspace: &Value,
) -> Value {
    json!({"runId":run,"planId":plan,"startedAt":started,"finishedAt":finished,"outcome":outcome,"workspaceStrategy":workspace["strategy"],"workspaceHandle":workspace["cwd"]})
}
