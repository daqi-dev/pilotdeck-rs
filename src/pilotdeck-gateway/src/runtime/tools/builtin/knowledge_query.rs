//! `knowledge_query` delegates to the configured external Knowledge module.
//! The model supplies only query parameters; host identity is either injected
//! here or bound by the authenticated module endpoint.
use super::{object, schema, string};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

const DESCRIPTION: &str = "Query the configured knowledge module for grounded evidence and citations. The host supplies tenant and agent scope; provide a natural-language query and optionally select knowledge_base_ids.";
const INPUT_SCHEMA: &str = r#"{"type":"object","required":["query"],"additionalProperties":true,"properties":{"query":{"type":"string"}}}"#;

/// The external-module host validates that `query` is declared before it
/// constructs this port. Transport/module failures stay as their original
/// `ToolError`, including provider-specific codes and details.
pub trait KnowledgeQueryPort: Send + Sync {
    fn call(
        &self,
        operation: &'static str,
        input: JsObject,
        authority_context: Option<JsObject>,
    ) -> ToolFuture<JsValue>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnowledgeScope {
    pub tenant_id: Option<JsString>,
    pub actor_user_id: Option<JsString>,
    pub agent_id: Option<JsString>,
    /// An authenticated read binding derives identity on the server. In this
    /// mode even identity-looking model input is rejected instead of ignored.
    pub credential_bound: bool,
}

pub struct KnowledgeQueryTool {
    fields: JsObject,
    port: Arc<dyn KnowledgeQueryPort>,
    scope: KnowledgeScope,
}

impl KnowledgeQueryTool {
    pub fn new(port: Arc<dyn KnowledgeQueryPort>, scope: KnowledgeScope) -> Self {
        Self {
            fields: object([
                ("name", string("knowledge_query")),
                ("description", string(DESCRIPTION)),
                ("kind", string("custom")),
                ("inputSchema", JsValue::Object(schema(INPUT_SCHEMA))),
            ]),
            port,
            scope,
        }
    }

    async fn query(
        &self,
        input: JsValue,
        authority_context: Option<JsObject>,
    ) -> Result<ToolOutput, ToolError> {
        let mut input = input.as_object().cloned().ok_or_else(|| {
            ToolError::new(
                "invalid_tool_input",
                "knowledge_query input must be an object.",
            )
        })?;
        if !input
            .get("query")
            .and_then(JsValue::as_string)
            .is_some()
        {
            return Err(ToolError::new(
                "invalid_tool_input",
                "knowledge_query requires a string query.",
            ));
        }

        const UNTRUSTED_AUTHORITY_FIELDS: [&str; 8] = [
            "tenantId",
            "tenant_id",
            "actorUserId",
            "actor_user_id",
            "agentId",
            "agent_id",
            "modelConfigId",
            "model_config_id",
        ];
        if self.scope.credential_bound
            && UNTRUSTED_AUTHORITY_FIELDS
                .iter()
                .any(|key| input.contains_key(*key))
        {
            return Err(ToolError::new(
                "MODULE_PROTOCOL_INCOMPATIBLE",
                "Knowledge query cannot supply identity or an SD model selection.",
            ));
        }

        if !self.scope.credential_bound {
            for (key, value) in [
                ("tenantId", self.scope.tenant_id.as_ref()),
                ("actorUserId", self.scope.actor_user_id.as_ref()),
                ("agentId", self.scope.agent_id.as_ref()),
            ] {
                if let Some(value) = value {
                    input.insert(key.into(), JsValue::String(value.clone()));
                }
            }
        }

        let result = self
            .port
            .call("query", input, authority_context)
            .await?;
        Ok(ToolOutput {
            content: vec![JsValue::Object(object([
                ("type", string("json")),
                ("value", result.clone()),
            ]))],
            data: Some(result),
            ..Default::default()
        })
    }
}

impl ToolDefinition for KnowledgeQueryTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}

impl ExecutableTool for KnowledgeQueryTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let authority_context = context
            .fields
            .get("knowledgeAuthority")
            .and_then(JsValue::as_object)
            .cloned();
        let tool = self.clone_for_call();
        Box::pin(async move { tool.query(input, authority_context).await })
    }
}

impl KnowledgeQueryTool {
    fn clone_for_call(&self) -> Self {
        Self {
            fields: self.fields.clone(),
            port: self.port.clone(),
            scope: self.scope.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingPort {
        calls: Mutex<Vec<(&'static str, JsObject, Option<JsObject>)>>,
        error: Mutex<Option<ToolError>>,
    }
    impl KnowledgeQueryPort for RecordingPort {
        fn call(
            &self,
            operation: &'static str,
            input: JsObject,
            authority_context: Option<JsObject>,
        ) -> ToolFuture<JsValue> {
            self.calls
                .lock()
                .unwrap()
                .push((operation, input, authority_context));
            let error = self.error.lock().unwrap().take();
            Box::pin(async move {
                tokio::task::yield_now().await;
                match error {
                    Some(error) => Err(error),
                    None => Ok(JsValue::Object(object([("answer", string("grounded"))]))),
                }
            })
        }
    }

    #[tokio::test]
    async fn injects_trusted_scope_and_returns_identical_json_and_data() {
        let port = Arc::new(RecordingPort::default());
        let tool = KnowledgeQueryTool::new(
            port.clone(),
            KnowledgeScope {
                tenant_id: Some("tenant-host".into()),
                actor_user_id: Some("actor-host".into()),
                agent_id: Some("agent-host".into()),
                credential_bound: false,
            },
        );
        let output = tool
            .query(
                JsValue::Object(object([
                    ("query", string("what changed?")),
                    ("tenantId", string("spoofed")),
                    ("knowledge_base_ids", JsValue::Array(vec![string("kb-1")])),
                ])),
                None,
            )
            .await
            .unwrap();
        let calls = port.calls.lock().unwrap();
        assert_eq!(calls[0].0, "query");
        assert_eq!(calls[0].1.get("tenantId"), Some(&string("tenant-host")));
        assert_eq!(calls[0].1.get("actorUserId"), Some(&string("actor-host")));
        assert_eq!(calls[0].1.get("agentId"), Some(&string("agent-host")));
        assert_eq!(calls[0].2, None);
        assert_eq!(output.content[0].get("value"), output.data.as_ref());
    }

    #[tokio::test]
    async fn credential_binding_rejects_model_authority_and_preserves_module_error() {
        let port = Arc::new(RecordingPort::default());
        let tool = KnowledgeQueryTool::new(
            port.clone(),
            KnowledgeScope {
                credential_bound: true,
                ..Default::default()
            },
        );
        let rejected = tool
            .query(
                JsValue::Object(object([
                    ("query", string("q")),
                    ("model_config_id", string("untrusted")),
                ])),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(rejected.code, "MODULE_PROTOCOL_INCOMPATIBLE");
        assert!(port.calls.lock().unwrap().is_empty());

        *port.error.lock().unwrap() = Some(ToolError::new("provider_denied", "denied by module"));
        let forwarded = tool
            .query(JsValue::Object(object([("query", string("q"))])), None)
            .await
            .unwrap_err();
        assert_eq!(forwarded.code, "provider_denied");
        assert!(forwarded.message.equals_str("denied by module"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_forwards_turn_authority_to_port_after_yield() {
        let port = Arc::new(RecordingPort::default());
        let tool = KnowledgeQueryTool::new(port.clone(), KnowledgeScope::default());
        let authority = object([
            ("sessionKey", string("sess")),
            ("snapshotId", string("snap-1")),
            ("registryGeneration", JsValue::Number(3.0)),
        ]);
        let output = tool
            .execute(
                JsValue::Object(object([("query", string("facts"))])),
                ToolRunContext::new(Arc::new(ToolRuntimeContext {
                    fields: object([(
                        "knowledgeAuthority",
                        JsValue::Object(authority.clone()),
                    )]),
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
                })),
            )
            .await
            .unwrap();
        assert!(output.data.is_some());
        let calls = port.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let forwarded = calls[0].2.as_ref().expect("authority forwarded");
        assert_eq!(forwarded.get("snapshotId"), Some(&string("snap-1")));
    }
}
