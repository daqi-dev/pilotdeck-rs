use super::{AlwaysOnError, now, report, report_metadata, storage::Store, workspace};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Mutex;

const ALL_ALWAYS_ON_TOOL_NAMES: &[&str] = &[
    "always_on_discovery_plan",
    "always_on_report",
    "always_on_prepare_workspace",
    "always_on_read_chat_history",
];

fn schema_for(name: &str) -> Value {
    match name {
        "always_on_discovery_plan" => json!({"name":name,"aliases":["AlwaysOnDiscoveryPlan"],"description":"Save the single discovery plan for this Always-On fire.","kind":"session","requiredRuntimeCapabilities":["always_on_run_context"],"readOnly":false,"concurrencySafe":false,"inputSchema":{"type":"object","required":["title","summary","rationale","dedupeKey","content"],"additionalProperties":false,"properties":{"title":{"type":"string"},"summary":{"type":"string"},"rationale":{"type":"string"},"dedupeKey":{"type":"string"},"content":{"type":"string"}}}}),
        "always_on_read_chat_history" => json!({"name":name,"aliases":["AlwaysOnReadChatHistory"],"description":"Read a user chat session from the discovery digest.","kind":"session","requiredRuntimeCapabilities":["always_on_run_context"],"readOnly":true,"concurrencySafe":true,"inputSchema":{"type":"object","required":["sessionId"],"additionalProperties":false,"properties":{"sessionId":{"type":"string"}}}}),
        "always_on_prepare_workspace" => json!({"name":name,"aliases":["AlwaysOnPrepareWorkspace"],"description":"Prepare an isolated workspace for this Always-On execution.","kind":"session","requiredRuntimeCapabilities":["always_on_run_context"],"readOnly":false,"concurrencySafe":false,"inputSchema":{"type":"object","required":["strategy"],"additionalProperties":false,"properties":{"strategy":{"type":"string","enum":["git-worktree","snapshot-copy","auto"]}}}}),
        _ => json!({"name":name,"aliases":["AlwaysOnReport"],"description":"Persist the work report for this Always-On execution.","kind":"session","requiredRuntimeCapabilities":["always_on_run_context"],"readOnly":false,"concurrencySafe":false,"inputSchema":{"type":"object","required":["content"],"additionalProperties":false,"properties":{"content":{"type":"string"}}}}),
    }
}

/// Native tool context supplied only to the owning phase's session.
pub struct RunTools {
    store: Store,
    run: String,
    phase: String,
    plan: Value,
    workspace: Mutex<Value>,
    report: Mutex<Option<PathBuf>>,
    started: String,
    submission: Mutex<Option<Value>>,
    aliases: Mutex<Value>,
    prepare_gate: tokio::sync::Mutex<()>,
    workspace_host: Option<std::sync::Arc<dyn super::AlwaysOnHost>>,
}
impl RunTools {
    pub(super) fn new(
        store: Store,
        run: &str,
        phase: &str,
        plan: Value,
        workspace: Value,
        started: String,
    ) -> Self {
        Self {
            store,
            run: run.into(),
            phase: phase.into(),
            plan,
            workspace: Mutex::new(workspace),
            report: Mutex::new(None),
            started,
            submission: Mutex::new(None),
            aliases: Mutex::new(serde_json::json!({})),
            prepare_gate: tokio::sync::Mutex::new(()),
            workspace_host: None,
        }
    }
    pub(super) fn with_workspace_host(
        mut self,
        host: std::sync::Arc<dyn super::AlwaysOnHost>,
    ) -> Self {
        self.workspace_host = Some(host);
        self
    }
    pub fn tool_names(&self) -> &'static [&'static str] {
        ALL_ALWAYS_ON_TOOL_NAMES
    }
    pub fn schemas(&self) -> Vec<Value> {
        ALL_ALWAYS_ON_TOOL_NAMES
            .iter()
            .map(|name| schema_for(name))
            .collect()
    }
    pub async fn call(&self, name: &str, input: &Value) -> Result<Value, AlwaysOnError> {
        if !input.is_object() {
            return Err(AlwaysOnError::new(
                "invalid_params",
                "Tool input must be an object",
            ));
        }
        match name {
            "always_on_discovery_plan" | "AlwaysOnDiscoveryPlan" if self.phase == "discovery" => {
                let _lock = self.prepare_gate.lock().await;
                if self.submitted_plan().is_some() {
                    return Err(AlwaysOnError::new(
                        "tool_execution_failed",
                        "plan_quota_exhausted: Always-On discovery permits at most one plan per fire.",
                    ));
                }
                for key in ["title", "summary", "rationale", "dedupeKey", "content"] {
                    if !input[key].is_string() {
                        return Err(AlwaysOnError::new(
                            "invalid_params",
                            format!("{key} must be a string"),
                        ));
                    }
                }
                if input.as_object().is_some_and(|o| o.len() != 5) {
                    return Err(AlwaysOnError::new(
                        "invalid_params",
                        "Unknown discovery plan fields",
                    ));
                }
                let (title, metadata, markdown) = super::plan::parse(
                    input["content"].as_str().expect("checked"),
                )
                .map_err(|e| {
                    AlwaysOnError::new(
                        "tool_execution_failed",
                        format!("plan_invalid: {}", e.message),
                    )
                })?;
                let id = metadata["id"].as_str().expect("parser checked");
                let relative = format!("plans/{}.md", super::storage::safe_id(id));
                super::storage::write_atomic(
                    &self.store.root.join(&relative),
                    markdown.as_bytes(),
                )?;
                let text = |key: &str| input[key].as_str().unwrap_or("").trim().to_owned();
                let input_title = text("title");
                let dedupe = text("dedupeKey");
                let record = json!({"id":id,"title":if input_title.is_empty(){title}else{input_title},"createdAt":now(),"status":"ready","summary":text("summary"),"rationale":text("rationale"),"dedupeKey":if dedupe.is_empty(){metadata["dedupeKey"].as_str().unwrap_or("")}else{&dedupe},"sourceRunId":metadata["sourceRunId"],"planFilePath":relative});
                self.store.insert_record("plans", record.clone())?;
                *self.submission.lock().unwrap_or_else(|e| e.into_inner()) = Some(record.clone());
                let data = json!({"ok":true,"planId":id,"planFilePath":record["planFilePath"],"dedupeKey":record["dedupeKey"]});
                Ok(
                    json!({"content":[{"type":"text","text":format!("Plan saved as {id} ({}).",record["dedupeKey"].as_str().unwrap_or(""))},{"type":"json","value":data}],"data":data,"metadata":{"runId":self.run}}),
                )
            }
            "always_on_read_chat_history" | "AlwaysOnReadChatHistory"
                if self.phase == "discovery" =>
            {
                let id = input["sessionId"]
                    .as_str()
                    .ok_or_else(|| AlwaysOnError::new("invalid_params", "sessionId required"))?;
                if input.as_object().is_some_and(|o| o.len() != 1) {
                    return Err(AlwaysOnError::new(
                        "invalid_params",
                        "Unknown chat history fields",
                    ));
                }
                let resolved = self.aliases.lock().unwrap_or_else(|e| e.into_inner())[id]
                    .as_str()
                    .unwrap_or(id)
                    .to_owned();
                let data =
                    super::chat::conversation(&self.store.home, &self.store.project, &resolved)?;
                Ok(
                    json!({"content":[{"type":"text","text":format!("Session \"{}\" — {} messages.",data["title"].as_str().unwrap_or(""),data["messageCount"])},{"type":"json","value":data}],"data":data,"metadata":{"runId":self.run}}),
                )
            }
            "always_on_prepare_workspace" | "AlwaysOnPrepareWorkspace"
                if self.phase == "workspace" =>
            {
                let _lock = self.prepare_gate.lock().await;
                if self.prepared_workspace().is_some() {
                    return Err(AlwaysOnError::new(
                        "tool_execution_failed",
                        "workspace_already_prepared: a workspace was already created for this run.",
                    ));
                }
                let strategy = input["strategy"]
                    .as_str()
                    .ok_or_else(|| AlwaysOnError::new("invalid_params", "strategy required"))?;
                if input.as_object().is_some_and(|o| o.len() != 1) {
                    return Err(AlwaysOnError::new(
                        "invalid_params",
                        "Unknown workspace tool fields",
                    ));
                }
                let workspace = workspace::prepare_notified(
                    &self.store,
                    &self.run,
                    self.plan["title"].as_str().unwrap_or(""),
                    strategy,
                    self.workspace_host.as_deref(),
                )
                .await?;
                let data = json!({"ok":true,"strategy":workspace["strategy"],"cwd":workspace["cwd"],"reused":false});
                *self.workspace.lock().unwrap_or_else(|e| e.into_inner()) = workspace;
                Ok(
                    json!({"content":[{"type":"text","text":format!("Workspace prepared at {} ({}).", data["cwd"].as_str().unwrap_or(""), data["strategy"].as_str().unwrap_or(""))},{"type":"json","value":data}],"data":data,"metadata":{"runId":self.run}}),
                )
            }
            "always_on_report" | "AlwaysOnReport" if self.phase == "execute" => Err(AlwaysOnError::new(
                "tool_execution_failed",
                "always_on_report called in a context without a reportStore.",
            )),
            "always_on_report" | "AlwaysOnReport" if self.phase == "report" => {
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| AlwaysOnError::new("invalid_params", "content required"))?;
                if input.as_object().is_some_and(|o| o.len() != 1) {
                    return Err(AlwaysOnError::new(
                        "invalid_params",
                        "Unknown report tool fields",
                    ));
                }
                let workspace = self
                    .workspace
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let metadata = report_metadata(
                    &self.run,
                    self.plan["id"].as_str().unwrap_or(""),
                    &self.started,
                    &now(),
                    "executed",
                    &workspace,
                );
                let (markdown, fallbacks) = report::normalize(content, &metadata);
                let path = self.store.report(&self.run, &markdown)?;
                *self.report.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.clone());
                let data = json!({"ok":true,"reportFilePath":path,"fallbacks":fallbacks});
                Ok(
                    json!({"content":[{"type":"text","text":format!("Work report saved at {}.",path.display())},{"type":"json","value":data}],"data":data,"metadata":{"runId":self.run,"planId":self.plan["id"]}}),
                )
            }
            "always_on_discovery_plan" | "AlwaysOnDiscoveryPlan" => Err(AlwaysOnError::new(
                "tool_execution_failed",
                "always_on_discovery_plan called outside of an Always-On discovery turn.",
            )),
            "always_on_read_chat_history" | "AlwaysOnReadChatHistory" => Err(
                AlwaysOnError::new(
                    "tool_execution_failed",
                    "always_on_read_chat_history is only available during Always-On discovery (Phase 1).",
                ),
            ),
            "always_on_report" | "AlwaysOnReport" => Err(AlwaysOnError::new(
                "tool_execution_failed",
                "always_on_report called outside of an Always-On report or execution turn.",
            )),
            "always_on_prepare_workspace" | "AlwaysOnPrepareWorkspace" => Err(AlwaysOnError::new(
                "tool_execution_failed",
                "always_on_prepare_workspace called outside of an Always-On workspace turn.",
            )),
            _ => Err(AlwaysOnError::new(
                "tool_execution_failed",
                format!("{name} called outside of its Always-On phase."),
            )),
        }
    }
    pub(super) fn set_digest(&self, digest: &Value) {
        *self.aliases.lock().unwrap_or_else(|e| e.into_inner()) = digest["aliasMap"].clone();
    }
    pub(super) fn submitted_plan(&self) -> Option<Value> {
        self.submission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn report_path(&self) -> Option<PathBuf> {
        self.report
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn prepared_workspace(&self) -> Option<Value> {
        let workspace = self.workspace.lock().unwrap_or_else(|e| e.into_inner());
        (!workspace.is_null()).then(|| workspace.clone())
    }
}
