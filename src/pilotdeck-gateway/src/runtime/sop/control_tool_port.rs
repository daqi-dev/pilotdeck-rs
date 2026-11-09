//! TS `SopControlToolPort.executeAll` / `SopAgentLoop.ts:722-749`.
use super::{
    authority_port::SopAuthorityResolve,
    authority_validate::{
        projection_stale_vs_resolve, validate_optional_knowledge_input, validate_projection_for_execute,
        validate_sop_authority_projection,
    },
    knowledge::write_knowledge_authority_for_sop_query,
    prepare_step::{
        production_refresh_optional_knowledge_from_current_step, SopPreparedStepStore,
    },
};
use crate::host::{
    modules::{
        capability::{self, ToolCall, ToolInvocationContext},
        tool_context::ToolRuntimeContext,
    },
    HostFuture, ModuleError,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// Session-scoped projection cache filled from `currentStep.optionalKnowledge`.
#[derive(Default)]
pub struct SopStepKnowledgeRegistry {
    inner: Mutex<HashMap<String, JsObject>>,
}

impl SopStepKnowledgeRegistry {
    pub fn sync_from_turn_input(&self, turn: &JsObject, session: &JsString) {
        if let Some(projection) = turn
            .get("sopOptionalKnowledge")
            .and_then(JsValue::as_object)
        {
            self.set_step_optional_knowledge(session, projection);
        }
    }

    pub fn set_step_optional_knowledge(&self, session: &JsString, optional: &JsObject) {
        if let Ok(key) = session.to_utf8() {
            self.inner
                .lock()
                .unwrap()
                .insert(key, optional.clone());
        }
    }

    pub fn optional_knowledge_for_session(&self, session: &JsString) -> Option<JsObject> {
        let key = session.to_utf8().ok()?;
        self.inner.lock().unwrap().get(&key).cloned()
    }
}

#[derive(Clone, Default)]
pub struct SopKnowledgeExecuteConfig {
    pub sessions_root: Option<PathBuf>,
    pub authority: Option<Arc<dyn SopAuthorityResolve>>,
}

pub struct SopKnowledgeToolExecution {
    inner: Arc<dyn capability::ToolExecution>,
    registry: Arc<SopStepKnowledgeRegistry>,
    prepared: Arc<SopPreparedStepStore>,
    config: SopKnowledgeExecuteConfig,
}

impl SopKnowledgeToolExecution {
    pub fn new(
        inner: Arc<dyn capability::ToolExecution>,
        registry: Arc<SopStepKnowledgeRegistry>,
        prepared: Arc<SopPreparedStepStore>,
        config: SopKnowledgeExecuteConfig,
    ) -> Self {
        Self {
            inner,
            registry,
            prepared,
            config,
        }
    }
}

impl capability::ToolCatalog for SopKnowledgeToolExecution {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        capability::ToolCatalog::list(self.inner.as_ref())
    }
}

fn clone_invocation(context: &ToolInvocationContext) -> ToolInvocationContext {
    ToolInvocationContext {
        runtime: Arc::clone(&context.runtime),
        execution: Arc::clone(&context.execution),
    }
}

impl capability::ToolExecution for SopKnowledgeToolExecution {
    fn refresh(&self) -> HostFuture<()> {
        capability::ToolExecution::refresh(self.inner.as_ref())
    }

    fn execute_all(
        &self,
        calls: Vec<ToolCall>,
        context: ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        let inner = self.inner.clone();
        let registry = self.registry.clone();
        let prepared = self.prepared.clone();
        let config = self.config.clone();
        Box::pin(async move {
            let session = match session_id_from_invocation(&context) {
                Some(session) => session,
                None if calls.iter().any(|c| c.name.equals_str("knowledge_query")) => {
                    return Ok(calls
                        .into_iter()
                        .map(|call| {
                            if call.name.equals_str("knowledge_query") {
                                knowledge_control_error(&call, "SOP_AUTHORITY_TOOL_UNAVAILABLE")
                            } else {
                                knowledge_control_error(&call, "SOP_AUTHORITY_UNAVAILABLE")
                            }
                        })
                        .collect());
                }
                None => return inner.execute_all(calls, context).await,
            };
            production_refresh_optional_knowledge_from_current_step(
                &prepared,
                &registry,
                &session,
            );
            let mut ordinary = Vec::new();
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                if !call.name.equals_str("knowledge_query") {
                    ordinary.push(call);
                    continue;
                }
                if !ordinary.is_empty() {
                    results.extend(
                        inner
                            .execute_all(std::mem::take(&mut ordinary), clone_invocation(&context))
                            .await?,
                    );
                }
                let Some(projection) = registry.optional_knowledge_for_session(&session) else {
                    results.push(knowledge_control_error(
                        &call,
                        "SOP_AUTHORITY_TOOL_UNAVAILABLE",
                    ));
                    continue;
                };
                match prepare_knowledge_query_call(
                    &call,
                    &projection,
                    clone_invocation(&context),
                    &config,
                )
                .await
                {
                    Ok(patched) => {
                        results.extend(inner.execute_all(vec![call], patched).await?);
                    }
                    Err(code) => results.push(knowledge_control_error(&call, &code)),
                }
            }
            if !ordinary.is_empty() {
                results.extend(inner.execute_all(ordinary, context).await?);
            }
            Ok(results)
        })
    }
}

async fn prepare_knowledge_query_call(
    call: &ToolCall,
    projection: &JsObject,
    context: ToolInvocationContext,
    config: &SopKnowledgeExecuteConfig,
) -> Result<ToolInvocationContext, String> {
    validate_optional_knowledge_input(&call.input)
        .map_err(|code| code.to_utf8().unwrap_or_default())?;
    validate_projection_for_execute(projection).map_err(|code| code.to_utf8().unwrap_or_default())?;
    let root = config
        .sessions_root
        .as_deref()
        .ok_or_else(|| "SOP_AUTHORITY_STATE_MISSING".to_string())?;
    let ctx = projection
        .get("context")
        .and_then(JsValue::as_object)
        .ok_or_else(|| "SOP_AUTHORITY_RESPONSE_INVALID".to_string())?;
    let session_key = ctx
        .get("sessionKey")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .ok_or_else(|| "SOP_AUTHORITY_RESPONSE_INVALID".to_string())?;
    let expected = ctx
        .get("expectedRevision")
        .cloned()
        .ok_or_else(|| "SOP_AUTHORITY_RESPONSE_INVALID".to_string())?;
    super::check_sop_authority_revision(root, &session_key, &expected)
        .await
        .map_err(|e| e.code)?;
    let authority = config
        .authority
        .as_ref()
        .ok_or_else(|| "SOP_AUTHORITY_UNAVAILABLE".to_string())?;
    let fresh = authority
        .resolve(ctx.clone())
        .await
        .map_err(|code| code.to_utf8().unwrap_or_default())?;
    validate_sop_authority_projection(&JsValue::Object(fresh.clone()), ctx)
        .map_err(|code| code.to_utf8().unwrap_or_default())?;
    if projection_stale_vs_resolve(projection, &fresh) {
        return Err("SOP_AUTHORITY_STALE".to_string());
    }
    super::check_sop_authority_revision(root, &session_key, &expected)
        .await
        .map_err(|e| e.code)?;
    patch_knowledge_authority_on_invocation(context, projection)
        .map_err(|e| String::from_utf16_lossy(e.message.units()))
}

fn knowledge_control_error(call: &ToolCall, code: &str) -> JsObject {
    let mut error = JsObject::new();
    error.insert("code".into(), JsValue::String(JsString::from(code)));
    error.insert("message".into(), JsValue::String(JsString::from(code)));
    let mut result = JsObject::new();
    result.insert("type".into(), JsValue::String(JsString::from("error")));
    result.insert("toolCallId".into(), JsValue::String(call.id.clone()));
    result.insert("toolName".into(), JsValue::String(call.name.clone()));
    result.insert("error".into(), JsValue::Object(error));
    result
}

fn session_id_from_invocation(context: &ToolInvocationContext) -> Option<JsString> {
    let host = context.runtime.downcast_ref::<ToolRuntimeContext>()?;
    host.fields
        .get("sessionId")
        .and_then(JsValue::as_string)
        .cloned()
}

fn patch_knowledge_authority_on_invocation(
    context: ToolInvocationContext,
    projection: &JsObject,
) -> Result<ToolInvocationContext, ModuleError> {
    let host = context
        .runtime
        .downcast_ref::<ToolRuntimeContext>()
        .ok_or_else(|| ModuleError::new("SOP knowledge port requires ToolRuntimeContext."))?;
    let mut fields = host.fields.clone();
    write_knowledge_authority_for_sop_query(&mut fields, projection);
    let runtime = Arc::new(ToolRuntimeContext {
        fields,
        files: host.files.clone(),
        abort_signal: host.abort_signal.clone(),
        model: host.model.clone(),
        elicitation: host.elicitation.clone(),
        user_dialog: host.user_dialog.clone(),
        audit_recorder: host.audit_recorder.clone(),
        file_history: host.file_history.clone(),
        file_update_notifier: host.file_update_notifier.clone(),
        now: host.now.clone(),
        progress: host.progress.clone(),
        subagent: host.subagent.clone(),
        plan_todo: host.plan_todo.clone(),
        goal: host.goal.clone(),
        plan_directory: host.plan_directory.clone(),
    }) as capability::RuntimeContext;
    Ok(ToolInvocationContext {
        runtime,
        execution: context.execution,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::{
        capability::{ToolCatalog, ToolExecution, ToolInvocationContext},
        tool_context::{ExecutionContext, ToolFileState, ToolRuntimeContext},
    };
    use crate::runtime::sop::{
        authority_port::SopAuthorityResolve,
        prepare_step::{
            production_prepare_current_step_optional_knowledge, ProductionPrepareInput,
            SopPreparedStepStore,
        },
        store,
    };
    use pilotdeck_protocol::{parse_json, stringify_json, JsObject, JsString, JsValue};
    use std::fs;
    use std::sync::{Arc, Mutex};

    fn s(value: &str) -> JsValue {
        JsValue::String(JsString::from(value))
    }

    fn step_with_projection() -> JsObject {
        let mut context = JsObject::new();
        context.insert("sessionKey".into(), s("sess-prod"));
        context.insert("projectKey".into(), s("proj"));
        context.insert("expectedRevision".into(), JsValue::Number(2.0));
        context.insert("requestId".into(), s("req-sop"));
        let mut cap = JsObject::new();
        cap.insert(
            "operation".into(),
            JsValue::String(JsString::from("knowledge.search/v1")),
        );
        cap.insert(
            "resourceType".into(),
            JsValue::String(JsString::from("knowledge_base")),
        );
        cap.insert("resourceId".into(), s("kb"));
        cap.insert("required".into(), JsValue::Bool(false));
        cap.insert(
            "providerModuleId".into(),
            JsValue::String(JsString::from("knowledge.local")),
        );
        cap.insert(
            "providerVersion".into(),
            JsValue::String(JsString::from("1")),
        );
        cap.insert(
            "selectionMode".into(),
            JsValue::String(JsString::from("current")),
        );
        let mut projection = JsObject::new();
        projection.insert("context".into(), JsValue::Object(context));
        projection.insert("sopId".into(), s("sop"));
        projection.insert("sopVersion".into(), s("1.0.0"));
        projection.insert("nodeId".into(), s("lookup"));
        projection.insert("snapshotId".into(), s("snap-prod"));
        projection.insert("registryGeneration".into(), JsValue::Number(11.0));
        projection.insert(
            "optionalCapabilities".into(),
            JsValue::Array(vec![JsValue::Object(cap)]),
        );
        projection
    }

    struct ResolveSame(JsObject);

    impl SopAuthorityResolve for ResolveSame {
        fn resolve(
            &self,
            context: JsObject,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<JsObject, JsString>> + Send + '_>,
        > {
            let mut projection = self.0.clone();
            let mut ctx = projection
                .get("context")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            for key in ["sessionKey", "projectKey", "expectedRevision", "requestId"] {
                if let Some(value) = context.get(key) {
                    ctx.insert(key.into(), value.clone());
                }
            }
            projection.insert("context".into(), JsValue::Object(ctx));
            Box::pin(async move { Ok(projection) })
        }
    }

    fn write_active_state(root: &std::path::Path, session: &str) {
        fs::create_dir_all(root).unwrap();
        let state = parse_json(
            r#"{"schemaVersion":3,"sessionId":"sess","revision":2,"bundle":{"sops":[{"id":"sop","skill_id":"sop","version":"1.0.0","content":{"nodes":[{"node_id":"lookup","type":"response"}]}}]},"state":{"active_skill_id":"sop","active_step_id":"lookup","status":"active"}}"#,
        )
        .unwrap();
        let mut doc = state.as_object().cloned().unwrap();
        doc.insert("sessionId".into(), JsValue::String(JsString::from(session)));
        fs::write(
            store::path(root, session),
            stringify_json(&JsValue::Object(doc)).unwrap().unwrap(),
        )
        .unwrap();
    }

    struct CaptureAuthority {
        authority: Arc<Mutex<Option<JsObject>>>,
    }

    impl ToolCatalog for CaptureAuthority {
        fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
            Box::pin(async { Ok(vec![]) })
        }
    }

    impl ToolExecution for CaptureAuthority {
        fn execute_all(
            &self,
            _: Vec<ToolCall>,
            context: ToolInvocationContext,
        ) -> HostFuture<Vec<JsObject>> {
            let capture = self.authority.clone();
            Box::pin(async move {
                let host = context
                    .runtime
                    .downcast_ref::<ToolRuntimeContext>()
                    .expect("host");
                *capture.lock().unwrap() = host
                    .fields
                    .get("knowledgeAuthority")
                    .and_then(JsValue::as_object)
                    .cloned();
                Ok(vec![])
            })
        }
    }

    #[tokio::test]
    async fn execute_all_reads_optional_knowledge_from_production_prepare_path() {
        let dir = std::env::temp_dir().join(format!("pdrs-sop-exec-{}", uuid::Uuid::new_v4()));
        let session_key = "sess-prod";
        write_active_state(&dir, session_key);
        let registry = Arc::new(SopStepKnowledgeRegistry::default());
        let prepared = Arc::new(SopPreparedStepStore::default());
        let session = JsString::from(session_key);
        let mut turn = JsObject::new();
        let projection = step_with_projection();
        production_prepare_current_step_optional_knowledge(ProductionPrepareInput {
            sessions_root: &dir,
            session_id: &session,
            turn_id: &JsString::from("turn-1"),
            project_key: "proj",
            registry: &registry,
            prepared: prepared.as_ref(),
            turn: &mut turn,
            authority_binding: Some(&JsValue::Bool(true)),
            authority: Some(Arc::new(ResolveSame(projection.clone()))),
            knowledge_query_registered: true,
        })
        .await
        .unwrap();
        let captured = Arc::new(Mutex::new(None));
        let inner = Arc::new(CaptureAuthority {
            authority: captured.clone(),
        });
        let port = SopKnowledgeToolExecution::new(
            inner,
            registry,
            prepared,
            SopKnowledgeExecuteConfig {
                sessions_root: Some(dir.clone()),
                authority: Some(Arc::new(ResolveSame(projection))),
            },
        );
        let mut fields = JsObject::new();
        fields.insert("sessionId".into(), JsValue::String(session.clone()));
        let runtime = Arc::new(ToolRuntimeContext {
            fields,
            files: ToolFileState {
                read_file_state: Arc::new(Mutex::new(JsObject::new())),
                write_snapshots: Arc::new(Mutex::new(JsObject::new())),
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
        });
        let context = ToolInvocationContext {
            runtime: runtime as capability::RuntimeContext,
            execution: Arc::new(ExecutionContext {
                fields: JsObject::new(),
                abort_signal: None,
            }) as capability::RuntimeContext,
        };
        let call = ToolCall {
            id: JsString::from("c1"),
            name: JsString::from("knowledge_query"),
            input: JsValue::Object({
                let mut input = JsObject::new();
                input.insert("query".into(), s("hello"));
                input
            }),
        };
        port.execute_all(vec![call], context).await.unwrap();
        let authority = captured.lock().unwrap().take().expect("patched");
        assert_eq!(authority.get("sessionKey"), Some(&s("sess-prod")));
        assert_eq!(authority.get("snapshotId"), Some(&s("snap-prod")));
        let _ = fs::remove_dir_all(dir);
    }
}
