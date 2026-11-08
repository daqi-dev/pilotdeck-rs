//! Session-scoped external knowledge tool; scheduler owns permission and hooks.
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    skills::{
        ExternalSkillBinding, ExternalSkillsPort, HyperModuleTransport, ModuleEnvironment,
        ModuleHttpTransport, ModuleInvocation,
    },
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub(super) struct KnowledgeTool {
    fields: JsObject,
    client: Arc<ExternalSkillsPort<dyn ModuleHttpTransport>>,
    sequence: AtomicU64,
    credential: bool,
    scope: JsValue,
}
impl KnowledgeTool {
    pub(super) fn new(
        binding: ExternalSkillBinding,
        scope: JsValue,
        transport: Arc<dyn ModuleHttpTransport>,
        env: Arc<dyn ModuleEnvironment>,
    ) -> Self {
        let credential = binding.credential_env.is_some();
        let fields = pilotdeck_protocol::parse_json(r#"{"name":"knowledge_query","kind":"custom","description":"Query the configured knowledge module for grounded evidence and citations. The host supplies tenant and agent scope; provide a natural-language query and optionally select knowledge_base_ids.","inputSchema":{"type":"object","required":["query"],"additionalProperties":true,"properties":{"query":{"type":"string"}}}}"#).expect("static schema").as_object().expect("object").clone();
        Self {
            fields,
            client: Arc::new(ExternalSkillsPort::with_environment(
                binding, transport, env,
            )),
            sequence: AtomicU64::new(0),
            credential,
            scope,
        }
    }
}
impl ToolDefinition for KnowledgeTool {
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
impl ExecutableTool for KnowledgeTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let client = self.client.clone();
        let id = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let credential = self.credential;
        let scope = self.scope.clone();
        Box::pin(async move {
            let mut payload = input.as_object().cloned().ok_or_else(|| {
                ToolError::new("invalid_input", "knowledge query must be an object")
            })?;
            if credential
                && [
                    "tenantId",
                    "tenant_id",
                    "actorUserId",
                    "actor_user_id",
                    "agentId",
                    "agent_id",
                    "modelConfigId",
                    "model_config_id",
                ]
                .iter()
                .any(|key| payload.contains_key(key))
            {
                return Err(ToolError::new(
                    "MODULE_PROTOCOL_INCOMPATIBLE",
                    "Knowledge query cannot supply identity or an SD model selection.",
                ));
            }
            if !credential {
                for key in ["tenantId", "actorUserId", "agentId"] {
                    if let Some(v) = scope
                        .get(key)
                        .filter(|v| crate::runtime::turn::submit::truthy(v))
                    {
                        payload.insert(key.into(), v.clone());
                    }
                }
            }
            let input = JsValue::Object(payload);
            let request_id = format!("knowledge-query-{id}");
            let operation = ModuleInvocation {
                module: "knowledge",
                run_id: "knowledge",
                operation_id: "knowledge",
                request_id: &request_id,
                operation: "query",
                input: &input,
                authority_context: context.knowledge_authority.as_ref(),
                on_dispatch: None,
            };
            let result = tokio::select! { biased; _=context.cancelled()=>return Err(ToolError::aborted()), result=client.call_module(operation)=>result.map_err(|e| ToolError::new(String::from_utf16_lossy(e.code.units()),e.message))? };
            let mut item = JsObject::new();
            item.insert("type".into(), "json".into());
            item.insert("value".into(), result.clone());
            Ok(ToolOutput {
                content: vec![JsValue::Object(item)],
                data: Some(result),
                ..ToolOutput::default()
            })
        })
    }
}

struct SnapshotEnvironment(crate::config::Env);
impl ModuleEnvironment for SnapshotEnvironment {
    fn var(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}
/// All inputs come from the same retained project generation; no runtime env reread.
pub(super) fn build(
    knowledge: &JsValue,
    profile: Option<&JsValue>,
    env: &crate::config::Env,
    proxy: Option<&crate::runtime::model::http::ProxySettings>,
) -> Result<
    (
        Arc<KnowledgeTool>,
        Option<Arc<crate::runtime::sop::authority::Authority>>,
    ),
    crate::skills::SkillError,
> {
    let binding = ExternalSkillBinding::from_value(knowledge)?;
    let transport: Arc<dyn ModuleHttpTransport> =
        Arc::new(HyperModuleTransport::with_proxy_settings(proxy, env));
    let env: Arc<dyn ModuleEnvironment> = Arc::new(SnapshotEnvironment(env.clone()));
    let authority = profile
        .and_then(|profile| crate::runtime::sop::authority::select_binding(knowledge, profile))
        .map(|binding| {
            Arc::new(crate::runtime::sop::authority::Authority::new(Arc::new(
                ExternalSkillsPort::with_environment(binding, transport.clone(), env.clone()),
            )))
        });
    Ok((
        Arc::new(KnowledgeTool::new(
            binding,
            knowledge.clone(),
            transport,
            env,
        )),
        authority,
    ))
}
