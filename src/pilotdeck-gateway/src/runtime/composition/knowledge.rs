//! `domainPorts.ts:107-161` and `ProjectSessionRuntimeBundle.ts:318-326`.
use super::HttpModuleClient;
use crate::{
    host::{HostError, HostErrorKind},
    runtime::tools::{
        ToolRegistry,
        builtin::knowledge_query::{KnowledgeQueryPort, KnowledgeQueryTool, KnowledgeScope},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};

const MODULE_HTTP_TRANSPORT: &str = "module-http-v2";

/// `isExternalModuleBinding` (types.ts:85-87).
pub fn is_external_module_binding(binding: &JsValue) -> bool {
    if matches!(binding, JsValue::Undefined | JsValue::Null) {
        return false;
    }
    binding.get("implementationId").is_some()
        && binding
            .get("transport")
            .and_then(JsValue::as_string)
            .is_some_and(|t| t.equals_str(MODULE_HTTP_TRANSPORT))
}

pub fn knowledge_scope_from_binding(binding: &JsValue) -> KnowledgeScope {
    KnowledgeScope {
        tenant_id: binding
            .get("tenantId")
            .and_then(JsValue::as_string)
            .cloned(),
        actor_user_id: binding
            .get("actorUserId")
            .and_then(JsValue::as_string)
            .cloned(),
        agent_id: binding
            .get("agentId")
            .and_then(JsValue::as_string)
            .cloned(),
        credential_bound: binding
            .get("credentialEnv")
            .and_then(JsValue::as_string)
            .is_some(),
    }
}

struct HttpKnowledgeModulePort {
    client: Arc<HttpModuleClient>,
    sequence: AtomicU64,
}

impl HttpKnowledgeModulePort {
    fn new(binding: JsValue) -> Self {
        Self {
            client: Arc::new(HttpModuleClient::new(binding)),
            sequence: AtomicU64::new(0),
        }
    }
}

impl KnowledgeQueryPort for HttpKnowledgeModulePort {
    fn call(
        &self,
        operation: &'static str,
        input: JsObject,
        authority_context: Option<JsObject>,
    ) -> crate::runtime::tools::ToolFuture<JsValue> {
        let client = self.client.clone();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        Box::pin(async move {
            client
                .call(
                    "knowledge",
                    "knowledge",
                    &format!("knowledge-{operation}-{sequence}"),
                    "knowledge",
                    operation,
                    input,
                    authority_context,
                )
                .await
                .map_err(|error| error.into_tool_error())
        })
    }
}

/// Registers `knowledge_query` when `modules.knowledge` is an external HTTP binding.
pub fn register_knowledge_query_tool(
    registry: &ToolRegistry,
    modules: &JsValue,
) -> Result<(), HostError> {
    let binding = modules
        .get("knowledge")
        .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined));
    let Some(binding) = binding else {
        return Ok(());
    };
    if !is_external_module_binding(binding) {
        return Ok(());
    }
    let name = JsString::from("knowledge_query");
    if registry.has(&name) {
        return Err(HostError::new(
            HostErrorKind::Protocol,
            "knowledge_query is already owned by this session.",
        ));
    }
    let scope = knowledge_scope_from_binding(binding);
    let port = Arc::new(HttpKnowledgeModulePort::new(binding.clone()));
    registry.register(Arc::new(KnowledgeQueryTool::new(port, scope)))?;
    Ok(())
}
