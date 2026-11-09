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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules::tool_context::{ToolFileState, ToolRuntimeContext};
    use crate::skills::{ModuleHttpFuture, ModuleHttpRequest, ModuleHttpResponse};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Transport(Mutex<Vec<JsValue>>);
    impl ModuleHttpTransport for Transport {
        fn send<'a>(&'a self, request: ModuleHttpRequest) -> ModuleHttpFuture<'a> {
            Box::pin(async move {
                let body = if request.method == "GET" {
                    r#"{"protocolVersion":"2.0","implementationId":"staffdeck.knowledge","contract":"staffdeck.knowledge/v1","transport":"module-http-v2","methods":["query"]}"#.to_string()
                } else {
                    let request =
                        pilotdeck_protocol::parse_json(std::str::from_utf8(&request.body).unwrap())
                            .unwrap();
                    let message = request
                        .get("messageId")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .to_utf8()
                        .unwrap();
                    let id = request
                        .get("requestId")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .to_utf8()
                        .unwrap();
                    self.0.lock().unwrap().push(request);
                    format!(
                        r#"{{"kind":"response","messageId":"reply","inReplyTo":"{message}","requestId":"{id}","ok":true,"payload":{{"result":{{"answer":"ok"}}}}}}"#
                    )
                };
                Ok(ModuleHttpResponse {
                    status: 200,
                    body: body.into_bytes(),
                })
            })
        }
    }
    struct Env;
    impl ModuleEnvironment for Env {
        fn var(&self, _: &str) -> Option<String> {
            Some("credential".into())
        }
    }
    fn context() -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(),
            files: ToolFileState {
                read_file_state: Arc::default(),
                write_snapshots: Arc::default(),
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
        }))
    }
    fn config(credential: bool) -> JsValue {
        let mut obj = crate::metadata::object([
            ("implementationId", "staffdeck.knowledge".into()),
            ("contract", "staffdeck.knowledge/v1".into()),
            ("transport", "module-http-v2".into()),
            ("endpoint", "https://fixture.invalid".into()),
            ("methods", JsValue::Array(vec!["query".into()])),
            ("tenantId", "tenant".into()),
            ("actorUserId", "actor".into()),
            ("agentId", "agent".into()),
        ])
        .as_object()
        .unwrap()
        .clone();
        if credential {
            obj.insert("credentialEnv".into(), "TOKEN".into());
        }
        JsValue::Object(obj)
    }
    fn tool(credential: bool, transport: Arc<Transport>) -> KnowledgeTool {
        let scope = config(credential);
        KnowledgeTool::new(
            ExternalSkillBinding::from_value(&scope).unwrap(),
            scope,
            transport,
            Arc::new(Env),
        )
    }
    #[tokio::test]
    async fn knowledge_consumer_rejects_identity_and_injects_bound_scope() {
        let transport = Arc::new(Transport::default());
        let protected = tool(true, transport.clone());
        for key in [
            "tenantId",
            "tenant_id",
            "actorUserId",
            "actor_user_id",
            "agentId",
            "agent_id",
            "modelConfigId",
            "model_config_id",
        ] {
            let mut input = JsObject::new();
            input.insert("query".into(), "q".into());
            input.insert(key.into(), "forged".into());
            assert_eq!(
                protected
                    .execute(JsValue::Object(input), context())
                    .await
                    .unwrap_err()
                    .code,
                "MODULE_PROTOCOL_INCOMPATIBLE",
                "{key}"
            );
        }
        assert_eq!(
            protected
                .execute(JsValue::Null, context())
                .await
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert!(transport.0.lock().unwrap().is_empty());
        let query = crate::metadata::object([("query", "hello".into())]);
        let result = protected.execute(query.clone(), context()).await.unwrap();
        assert_eq!(
            result.data.unwrap().get("answer"),
            Some(&JsValue::from("ok"))
        );
        let plain = tool(false, transport.clone());
        plain.execute(query, context()).await.unwrap();
        let requests = transport.0.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let payload = requests[1].get("payload").unwrap().get("input").unwrap();
        assert_eq!(payload.get("tenantId"), Some(&JsValue::from("tenant")));
        assert_eq!(payload.get("actorUserId"), Some(&JsValue::from("actor")));
        assert_eq!(payload.get("agentId"), Some(&JsValue::from("agent")));
        assert!(
            requests[0]
                .get("payload")
                .unwrap()
                .get("input")
                .unwrap()
                .get("tenantId")
                .is_none()
        );
    }
    #[tokio::test]
    async fn fixed_ts_model_selection_keys_reach_real_knowledge_consumer() {
        let golden = pilotdeck_protocol::parse_json(include_str!(
            "../../tests/fixtures/sop-knowledge-authority/model-id-oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = golden.get("cases").unwrap() else {
            panic!("cases")
        };
        assert_eq!(cases.len(), 5);
        let mut accepted = 0;
        let mut rejected = 0;
        for case in cases {
            let id = case
                .get("id")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            let credential = id.starts_with("credential-");
            let mut scope = config(credential).as_object().unwrap().clone();
            for (key, value) in [
                ("tenantId", "trusted-tenant"),
                ("actorUserId", "trusted-user"),
                ("agentId", "trusted-agent"),
            ] {
                scope.insert(key.into(), value.into());
            }
            let scope = JsValue::Object(scope);
            let transport = Arc::new(Transport::default());
            let consumer = KnowledgeTool::new(
                ExternalSkillBinding::from_value(&scope).unwrap(),
                scope,
                transport.clone(),
                Arc::new(Env),
            );
            let answer = consumer
                .execute(case.get("input").unwrap().clone(), context())
                .await;
            let requests = transport.0.lock().unwrap();
            if credential {
                assert_eq!(
                    answer.unwrap_err().code,
                    case.get("error")
                        .unwrap()
                        .get("code")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .to_utf8()
                        .unwrap(),
                    "{id}"
                );
                assert!(requests.is_empty(), "{id}");
                rejected += 1;
            } else {
                answer.unwrap();
                assert_eq!(requests.len(), 1, "{id}");
                let JsValue::Array(wire) = case.get("wire").unwrap() else {
                    panic!("wire")
                };
                let post = wire
                    .iter()
                    .find(|row| row.get("method") == Some(&JsValue::from("POST")))
                    .unwrap();
                let expected = post
                    .get("body")
                    .unwrap()
                    .get("payload")
                    .unwrap()
                    .get("input")
                    .unwrap();
                let actual = requests[0].get("payload").unwrap().get("input").unwrap();
                assert_eq!(actual, expected, "{id}");
                accepted += 1;
            }
        }
        assert_eq!((accepted, rejected), (3, 2));
    }
    #[test]
    fn build_exposes_authority_only_for_matching_same_origin_agent() {
        let knowledge = config(true);
        let profile = crate::metadata::object([
            ("discoveryAgentId", "agent".into()),
            (
                "discoveryEndpoint",
                "https://fixture.invalid/discovery".into(),
            ),
        ]);
        let env = crate::config::Env::new();
        assert!(
            build(&knowledge, Some(&profile), &env, None)
                .unwrap()
                .1
                .is_some()
        );
        let other = crate::metadata::object([
            ("discoveryAgentId", "other".into()),
            (
                "discoveryEndpoint",
                "https://fixture.invalid/discovery".into(),
            ),
        ]);
        assert!(
            build(&knowledge, Some(&other), &env, None)
                .unwrap()
                .1
                .is_none()
        );
        assert!(
            build(&config(false), Some(&profile), &env, None)
                .unwrap()
                .1
                .is_none()
        );
    }
}
