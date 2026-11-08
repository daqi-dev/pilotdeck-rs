//! Owner-authorized knowledge projection for a StaffDeck SOP response node.
use super::common::{error, field, text};
use crate::{host::HostError, skills::ExternalSkillBinding};
use pilotdeck_protocol::JsValue;

/// Match the knowledge binding to the same project's SOP discovery identity.
/// Inputs are already captured from the selected project generation.
pub(crate) fn select_binding(
    knowledge: &JsValue,
    profile: &JsValue,
) -> Option<ExternalSkillBinding> {
    let s = |v: &JsValue, key: &str| field(v, key).as_string().and_then(|s| s.to_utf8().ok());
    if s(knowledge, "transport")?.as_str() != "module-http-v2"
        || s(knowledge, "implementationId")?.as_str() != "staffdeck.knowledge"
        || s(knowledge, "contract")?.as_str() != "staffdeck.knowledge/v1"
        || s(knowledge, "credentialEnv")?.is_empty()
    {
        return None;
    }
    let agent = s(knowledge, "agentId")?;
    if agent.is_empty() || Some(agent.clone()) != s(profile, "discoveryAgentId") {
        return None;
    }
    let endpoint = url::Url::parse(&s(knowledge, "endpoint")?).ok()?;
    let discovery = url::Url::parse(&s(profile, "discoveryEndpoint")?).ok()?;
    if endpoint.origin() != discovery.origin() {
        return None;
    }
    let mut binding = ExternalSkillBinding::from_value(knowledge).ok()?;
    binding.implementation_id = "staffdeck.sop-capability-authority".into();
    binding.contract = "staffdeck.sop-capability-authority/v1".into();
    binding.manifest_path = "/api/v1/sop-capability-authority/module-manifest".into();
    binding.call_path = format!(
        "/api/v1/agents/{}/sop-capability-authority/v2/module/call",
        crate::channel::http_client::encode_uri_component(&agent)
    );
    binding.methods = vec!["resolve".into()];
    Some(binding)
}

pub(super) fn validate_projection(value: &JsValue, context: &JsValue) -> Result<(), HostError> {
    let invalid = || {
        error(
            "SOP_AUTHORITY_RESPONSE_INVALID",
            "SOP_AUTHORITY_RESPONSE_INVALID",
        )
    };
    let equal_context = match (field(value, "context").as_object(), context.as_object()) {
        (Some(actual), Some(expected)) => {
            actual.len() == expected.len()
                && expected
                    .own_entries()
                    .iter()
                    .all(|(key, val)| actual.get_key(key) == Some(val))
        }
        _ => false,
    };
    if value.as_object().is_none()
        || !equal_context
        || ["sopId", "sopVersion", "nodeId", "snapshotId"]
            .iter()
            .any(|key| text(field(value, key)).is_none())
    {
        return Err(invalid());
    }
    let generation = field(value, "registryGeneration")
        .as_number()
        .ok_or_else(invalid)?;
    if !generation.is_finite() || generation < 0.0 || generation.fract() != 0.0 {
        return Err(invalid());
    }
    let JsValue::Array(capabilities) = field(value, "optionalCapabilities") else {
        return Err(invalid());
    };
    for capability in capabilities {
        if capability.as_object().is_none()
            || field(capability, "operation") != &JsValue::from("knowledge.search/v1")
            || field(capability, "resourceType") != &JsValue::from("knowledge_base")
            || text(field(capability, "resourceId")).is_none()
            || field(capability, "required") != &JsValue::Bool(false)
            || field(capability, "providerModuleId") != &JsValue::from("knowledge.local")
            || text(field(capability, "providerVersion")).is_none()
            || field(capability, "selectionMode") != &JsValue::from("current")
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(super) fn validate_input(value: &JsValue) -> Result<(), HostError> {
    let invalid = || error("SOP_AUTHORITY_INPUT_INVALID", "SOP_AUTHORITY_INPUT_INVALID");
    let object = value.as_object().ok_or_else(invalid)?;
    if object.own_entries().iter().any(|(key, _)| {
        ![
            "query",
            "knowledgeBaseIds",
            "knowledgeBaseVersionIds",
            "documentIds",
            "maxChunks",
        ]
        .iter()
        .any(|name| key.equals_str(name))
    }) || text(field(value, "query")).is_none()
    {
        return Err(invalid());
    }
    for key in ["knowledgeBaseIds", "knowledgeBaseVersionIds", "documentIds"] {
        let item = field(value, key);
        if !item.is_undefined()
            && !matches!(item, JsValue::Array(items) if items.iter().all(|v| text(v).is_some()))
        {
            return Err(invalid());
        }
    }
    let n = field(value, "maxChunks");
    if !n.is_undefined()
        && !n
            .as_number()
            .is_some_and(|n| (1.0..=12.0).contains(&n) && n.fract() == 0.0)
    {
        return Err(invalid());
    }
    Ok(())
}

/// Per-call trusted execution wrapper, constructed only by the admitted SOP owner.
pub(crate) struct AuthorityExecution {
    pub(crate) original: crate::host::modules::capability::RuntimeContext,
    pub(crate) authority: JsValue,
}

pub(crate) struct Authority {
    client:
        std::sync::Arc<crate::skills::ExternalSkillsPort<dyn crate::skills::ModuleHttpTransport>>,
}
impl Authority {
    pub(crate) fn new(
        client: std::sync::Arc<
            crate::skills::ExternalSkillsPort<dyn crate::skills::ModuleHttpTransport>,
        >,
    ) -> Self {
        Self { client }
    }
    pub(super) async fn resolve(
        &self,
        context: &JsValue,
        signal: &crate::host::Cancellation,
    ) -> Result<JsValue, HostError> {
        let session = field(context, "sessionKey")
            .as_string()
            .and_then(|v| v.to_utf8().ok())
            .ok_or_else(|| {
                error(
                    "SOP_AUTHORITY_UNAVAILABLE",
                    "Authority session cannot be represented by this host.",
                )
            })?;
        let request_id = field(context, "requestId")
            .as_string()
            .and_then(|v| v.to_utf8().ok())
            .ok_or_else(|| {
                error(
                    "SOP_AUTHORITY_UNAVAILABLE",
                    "Authority request cannot be represented by this host.",
                )
            })?;
        let run_id = format!("sop:{session}");
        if signal.is_cancelled() {
            return Err(error("MODULE_ABORTED", "Module request was cancelled before dispatch."));
        }
        let dispatched = std::sync::atomic::AtomicBool::new(false);
        let call = crate::skills::ModuleInvocation {
            module: "capability",
            run_id: &run_id,
            operation_id: "sop-authority.resolve",
            request_id: &request_id,
            operation: "resolve",
            input: context,
            authority_context: None,
            on_dispatch: Some(&dispatched),
        };
        let value = tokio::select! { biased; _=signal.cancelled()=>return Err(error("MODULE_ABORTED", if dispatched.load(std::sync::atomic::Ordering::SeqCst) { "Module request was cancelled." } else { "Module request was cancelled before dispatch." })), result=self.client.call_module(call)=>result.map_err(|e|error(&String::from_utf16_lossy(e.code.units()),e.message))? };
        validate_projection(&value, context)?;
        Ok(value)
    }
}

pub(super) fn checked_execution_authority(
    old: &JsValue,
    fresh: &JsValue,
) -> Result<JsValue, HostError> {
    let context = field(old, "context");
    validate_projection(fresh, context)?;
    for key in [
        "snapshotId",
        "sopId",
        "sopVersion",
        "nodeId",
        "registryGeneration",
    ] {
        if field(old, key) != field(fresh, key) {
            return Err(error("SOP_AUTHORITY_STALE", "SOP_AUTHORITY_STALE"));
        }
    }
    if !matches!(field(fresh,"optionalCapabilities"),JsValue::Array(v) if !v.is_empty()) {
        return Err(error("SOP_AUTHORITY_STALE", "SOP_AUTHORITY_STALE"));
    }
    let mut output = context.as_object().cloned().ok_or_else(|| {
        error(
            "SOP_AUTHORITY_RESPONSE_INVALID",
            "SOP_AUTHORITY_RESPONSE_INVALID",
        )
    })?;
    output.insert("snapshotId".into(), field(old, "snapshotId").clone());
    output.insert(
        "registryGeneration".into(),
        field(old, "registryGeneration").clone(),
    );
    Ok(JsValue::Object(output))
}

pub(super) fn optional_schema() -> JsValue {
    pilotdeck_protocol::parse_json(r#"{"type":"object","required":["query"],"additionalProperties":false,"properties":{"query":{"type":"string"},"knowledgeBaseIds":{"type":"array","items":{"type":"string"}},"knowledgeBaseVersionIds":{"type":"array","items":{"type":"string"}},"documentIds":{"type":"array","items":{"type":"string"}},"maxChunks":{"type":"integer","minimum":1,"maximum":12}}}"#).expect("static optional knowledge schema")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_ts_projection_and_input_cases() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/sop-knowledge-authority/oracle.json"
        ))
        .expect("TS oracle");
        let context = field(&oracle, "context");
        let JsValue::Array(cases) = field(&oracle, "cases") else {
            panic!("cases")
        };
        assert_eq!(cases.len(), 7);
        for case in cases {
            let expected = field(case, "result").as_string().expect("result");
            assert_eq!(
                validate_projection(field(case, "value"), context).is_ok(),
                expected.equals_str("ok"),
                "{:?}",
                field(case, "name")
            );
        }
        let JsValue::Array(inputs) = field(&oracle, "inputs") else {
            panic!("inputs")
        };
        assert_eq!(inputs.len(), 6);
        for case in inputs {
            let expected = field(case, "result").as_string().expect("result");
            assert_eq!(
                validate_input(field(case, "value")).is_ok(),
                expected.equals_str("ok"),
                "{:?}",
                field(case, "name")
            );
        }
    }
    #[test]
    fn same_origin_and_agent_are_required_before_external_authority() {
        let parse = |v| pilotdeck_protocol::parse_json(v).unwrap();
        let k = parse(
            r#"{"implementationId":"staffdeck.knowledge","contract":"staffdeck.knowledge/v1","transport":"module-http-v2","endpoint":"https://host.example:443","credentialEnv":"SECRET_NAME","agentId":"a","methods":["query"]}"#,
        );
        let p =
            parse(r#"{"discoveryEndpoint":"https://host.example/else","discoveryAgentId":"a"}"#);
        let binding = select_binding(&k, &p).expect("matching identity and origin");
        assert_eq!(
            binding.implementation_id,
            "staffdeck.sop-capability-authority"
        );
        assert_eq!(
            binding.call_path,
            "/api/v1/agents/a/sop-capability-authority/v2/module/call"
        );
        let different =
            parse(r#"{"discoveryEndpoint":"https://host.example:444","discoveryAgentId":"a"}"#);
        assert!(select_binding(&k, &different).is_none());
        let different =
            parse(r#"{"discoveryEndpoint":"https://host.example","discoveryAgentId":"b"}"#);
        assert!(select_binding(&k, &different).is_none());
    }
    struct FakeEnv;
    impl crate::skills::ModuleEnvironment for FakeEnv {
        fn var(&self, key: &str) -> Option<String> {
            (key == "PDRS_TEST_KNOWLEDGE").then(|| "fixture-token".to_owned())
        }
    }
    #[derive(Default)]
    struct FakeTransport(std::sync::Mutex<Vec<crate::skills::ModuleHttpRequest>>);
    impl crate::skills::ModuleHttpTransport for FakeTransport {
        fn send<'a>(
            &'a self,
            request: crate::skills::ModuleHttpRequest,
        ) -> crate::skills::ModuleHttpFuture<'a> {
            Box::pin(async move {
                let authority = request.url.contains("sop-capability-authority");
                let response = if request.method == "GET" {
                    if authority {
                        r#"{"protocolVersion":"2.0","implementationId":"staffdeck.sop-capability-authority","contract":"staffdeck.sop-capability-authority/v1","transport":"module-http-v2","methods":["resolve"]}"#.to_owned()
                    } else {
                        r#"{"protocolVersion":"2.0","implementationId":"staffdeck.knowledge","contract":"staffdeck.knowledge/v1","transport":"module-http-v2","methods":["query"]}"#.to_owned()
                    }
                } else {
                    let body =
                        pilotdeck_protocol::parse_json(std::str::from_utf8(&request.body).unwrap())
                            .unwrap();
                    let message = field(&body, "messageId")
                        .as_string()
                        .unwrap()
                        .to_utf8()
                        .unwrap();
                    let request_id = field(&body, "requestId")
                        .as_string()
                        .unwrap()
                        .to_utf8()
                        .unwrap();
                    let result = if authority {
                        r#"{"context":{"sessionKey":"s","projectKey":"/p","expectedRevision":3,"requestId":"sop.authority:s:t:3"},"sopId":"a","sopVersion":"1","nodeId":"n","snapshotId":"snap","registryGeneration":1,"optionalCapabilities":[{"operation":"knowledge.search/v1","resourceType":"knowledge_base","resourceId":"r","required":false,"providerModuleId":"knowledge.local","providerVersion":"1","selectionMode":"current"}]}"#
                    } else {
                        r#"{"answer":"fixture"}"#
                    };
                    format!(
                        r#"{{"kind":"response","messageId":"reply-{message}","inReplyTo":"{message}","requestId":"{request_id}","ok":true,"payload":{{"result":{result}}}}}"#
                    )
                };
                self.0.lock().unwrap().push(request);
                Ok(crate::skills::ModuleHttpResponse {
                    status: 200,
                    body: response.into_bytes(),
                })
            })
        }
    }
    #[tokio::test]
    async fn authority_and_knowledge_wire_match_fixed_ts() {
        use crate::skills::{
            ExternalSkillBinding, ExternalSkillsPort, ModuleHttpTransport, ModuleInvocation,
        };
        let parse = |v| pilotdeck_protocol::parse_json(v).unwrap();
        let knowledge = parse(
            r#"{"implementationId":"staffdeck.knowledge","contract":"staffdeck.knowledge/v1","transport":"module-http-v2","endpoint":"https://fixture.invalid","credentialEnv":"PDRS_TEST_KNOWLEDGE","agentId":"a","manifestPath":"/api/v1/knowledge-module/module-manifest","callPath":"/api/v1/agents/a/knowledge-module/v2/module/call","methods":["query"]}"#,
        );
        let profile =
            parse(r#"{"discoveryEndpoint":"https://fixture.invalid/sop","discoveryAgentId":"a"}"#);
        let mock = std::sync::Arc::new(FakeTransport::default());
        let transport: std::sync::Arc<dyn ModuleHttpTransport> = mock.clone();
        let env = std::sync::Arc::new(FakeEnv);
        let authority = Authority::new(std::sync::Arc::new(ExternalSkillsPort::with_environment(
            select_binding(&knowledge, &profile).unwrap(),
            transport.clone(),
            env.clone(),
        )));
        let context = parse(
            r#"{"sessionKey":"s","projectKey":"/p","expectedRevision":3,"requestId":"sop.authority:s:t:3"}"#,
        );
        let projection = authority
            .resolve(&context, &crate::host::Cancellation::default())
            .await
            .unwrap();
        let scoped = checked_execution_authority(&projection, &projection).unwrap();
        let mut stale = projection.clone();
        super::super::common::set(&mut stale, "registryGeneration", JsValue::Number(2.0));
        assert!(checked_execution_authority(&projection, &stale).is_err());
        let client = ExternalSkillsPort::with_environment(
            ExternalSkillBinding::from_value(&knowledge).unwrap(),
            transport,
            env,
        );
        let input = parse(r#"{"query":"fixture"}"#);
        let value = client
            .call_module(ModuleInvocation {
                module: "knowledge",
                run_id: "knowledge",
                operation_id: "knowledge",
                request_id: "knowledge-query-1",
                operation: "query",
                input: &input,
                authority_context: Some(&scoped),
                on_dispatch: None,
            })
            .await
            .unwrap();
        assert_eq!(value, parse(r#"{"answer":"fixture"}"#));
        let oracle = parse(include_str!(
            "../../../tests/fixtures/sop-knowledge-authority/wire.json"
        ));
        let JsValue::Array(expected) = field(&oracle, "calls") else {
            panic!("calls")
        };
        let requests = mock.0.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(expected.len(), 4);
        for (actual, expected) in requests.iter().zip(expected) {
            assert_eq!(
                url::Url::parse(&actual.url).unwrap().path(),
                field(expected, "path")
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap()
            );
            assert_eq!(
                actual.method,
                field(expected, "method")
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap()
            );
            let authorization = actual.authorization.as_deref().unwrap_or("");
            let expected_authorization = field(expected, "authorization")
                .as_string()
                .and_then(|v| v.to_utf8().ok())
                .unwrap_or_default();
            assert_eq!(authorization, expected_authorization);
            let body = if actual.body.is_empty() {
                JsValue::Null
            } else {
                parse(std::str::from_utf8(&actual.body).unwrap())
            };
            assert_eq!(body, *field(expected, "body"));
        }
    }

    #[derive(Default)]
    struct StallTransport {
        started: std::sync::atomic::AtomicUsize,
        changed: tokio::sync::Notify,
    }
    impl crate::skills::ModuleHttpTransport for StallTransport {
        fn send<'a>(
            &'a self,
            _request: crate::skills::ModuleHttpRequest,
        ) -> crate::skills::ModuleHttpFuture<'a> {
            Box::pin(async move {
                self.started
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.changed.notify_waiters();
                std::future::pending().await
            })
        }
    }
    async fn wait_started(mock: &StallTransport, target: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let notified = mock.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if mock.started.load(std::sync::atomic::Ordering::SeqCst) >= target {
                    break;
                }
                notified.await;
            }
        })
        .await
        .expect("request reached transport");
    }
    #[tokio::test]
    async fn abort_drops_network_future_and_returns_shared_slot() {
        use crate::skills::{
            ExternalSkillBinding, ExternalSkillsPort, ModuleHttpTransport, ModuleInvocation,
        };
        let binding=ExternalSkillBinding::from_value(&pilotdeck_protocol::parse_json(r#"{"implementationId":"staffdeck.knowledge","contract":"staffdeck.knowledge/v1","transport":"module-http-v2","endpoint":"https://fixture.invalid","methods":["query"]}"#).unwrap()).unwrap();
        let mock = std::sync::Arc::new(StallTransport::default());
        let transport: std::sync::Arc<dyn ModuleHttpTransport> = mock.clone();
        let mut jobs = Vec::new();
        for _ in 0..4 {
            let port =
                std::sync::Arc::new(ExternalSkillsPort::new(binding.clone(), transport.clone()));
            jobs.push(tokio::spawn(async move {
                let input = JsValue::Null;
                let _ = port
                    .call_module(ModuleInvocation {
                        module: "knowledge",
                        run_id: "r",
                        operation_id: "o",
                        request_id: "q",
                        operation: "query",
                        input: &input,
                        authority_context: None,
                on_dispatch: None,
                    })
                    .await;
            }));
        }
        wait_started(&mock, 4).await;
        let mut short = binding.clone(); short.timeout_ms = 75.0;
        let queued = ExternalSkillsPort::new(short, transport.clone());
        let input = JsValue::Null;
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), queued.call_module(ModuleInvocation {
            module:"knowledge", run_id:"r", operation_id:"o", request_id:"queued", operation:"query", input:&input,
            authority_context:None, on_dispatch:None,
        })).await.expect("queue timeout must be bounded").expect_err("fifth request must time out before entering the transport");
        assert!(error.code.equals_str("MODULE_TIMEOUT"));
        assert_eq!(mock.started.load(std::sync::atomic::Ordering::SeqCst),4);
        let port = std::sync::Arc::new(ExternalSkillsPort::new(binding, transport));
        let fifth = tokio::spawn(async move {
            let input = JsValue::Null;
            let _ = port
                .call_module(ModuleInvocation {
                    module: "knowledge",
                    run_id: "r",
                    operation_id: "o",
                    request_id: "q",
                    operation: "query",
                    input: &input,
                    authority_context: None,
                on_dispatch: None,
                })
                .await;
        });
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                wait_started(&mock, 5)
            )
            .await
            .is_err()
        );
        let first = jobs.remove(0);
        first.abort();
        let _ = first.await;
        wait_started(&mock, 5).await;
        fifth.abort();
        for job in jobs {
            job.abort();
            let _ = job.await;
        }
    }
    #[tokio::test]
    async fn native_transport_rejects_an_oversize_successful_manifest() {
        use crate::skills::{
            ExternalSkillBinding, ExternalSkillsPort, HyperModuleTransport, ModuleInvocation,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            let prefix = r#"{"protocolVersion":"2.0","implementationId":"staffdeck.knowledge","contract":"staffdeck.knowledge/v1","transport":"module-http-v2","methods":["query"],"pad":""#;
            let suffix = "\"}";
            let length = prefix.len() + 8 * 1024 * 1024 + suffix.len();
            let head =
                format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(prefix.as_bytes()).await.unwrap();
            let chunk = vec![b'x'; 65536];
            for _ in 0..128 {
                stream.write_all(&chunk).await.unwrap();
            }
            stream.write_all(suffix.as_bytes()).await.unwrap();
        });
        let binding = ExternalSkillBinding::from_value(&crate::metadata::object([
            ("implementationId", "staffdeck.knowledge".into()),
            ("contract", "staffdeck.knowledge/v1".into()),
            ("transport", "module-http-v2".into()),
            ("endpoint", format!("http://{addr}").as_str().into()),
            ("methods", JsValue::Array(vec!["query".into()])),
        ]))
        .unwrap();
        let transport = std::sync::Arc::new(HyperModuleTransport::with_proxy_settings(
            None,
            &crate::config::Env::new(),
        ));
        let port = ExternalSkillsPort::new(binding, transport);
        let input = JsValue::Null;
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            port.call_module(ModuleInvocation {
                module: "knowledge",
                run_id: "r",
                operation_id: "o",
                request_id: "q",
                operation: "query",
                input: &input,
                authority_context: None,
                on_dispatch: None,
            }),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.code.equals_str("MODULE_PROTOCOL_INCOMPATIBLE"));
        server.await.unwrap();
    }
    struct AbortTransport { post_started: tokio::sync::Semaphore }
    impl crate::skills::ModuleHttpTransport for AbortTransport {
        fn send<'a>(&'a self, request: crate::skills::ModuleHttpRequest) -> crate::skills::ModuleHttpFuture<'a> {
            Box::pin(async move {
                if request.method == "GET" {
                    return Ok(crate::skills::ModuleHttpResponse { status:200, body:br#"{"protocolVersion":"2.0","implementationId":"staffdeck.sop-capability-authority","contract":"staffdeck.sop-capability-authority/v1","transport":"module-http-v2","methods":["resolve"]}"#.to_vec() });
                }
                self.post_started.add_permits(1);
                std::future::pending().await
            })
        }
    }
    #[tokio::test]
    async fn authority_cancel_distinguishes_before_and_after_dispatch() {
        use crate::skills::{ExternalSkillBinding,ExternalSkillsPort,ModuleHttpTransport};
        let binding=ExternalSkillBinding::from_value(&pilotdeck_protocol::parse_json(r#"{"implementationId":"staffdeck.sop-capability-authority","contract":"staffdeck.sop-capability-authority/v1","transport":"module-http-v2","endpoint":"https://fixture.invalid","methods":["resolve"]}"#).unwrap()).unwrap();
        let mock=std::sync::Arc::new(AbortTransport{post_started:tokio::sync::Semaphore::new(0)});
        let transport:std::sync::Arc<dyn ModuleHttpTransport>=mock.clone();
        let authority=std::sync::Arc::new(Authority::new(std::sync::Arc::new(ExternalSkillsPort::new(binding,transport))));
        let context=pilotdeck_protocol::parse_json(r#"{"sessionKey":"s","projectKey":"/p","expectedRevision":1,"requestId":"q"}"#).unwrap();
        let pre=crate::host::Cancellation::default();pre.cancel(None);
        assert_eq!(authority.resolve(&context,&pre).await.unwrap_err().message,"Module request was cancelled before dispatch.");
        let signal=crate::host::Cancellation::default();let child=signal.clone();let task=tokio::spawn(async move{authority.resolve(&context,&child).await});
        tokio::time::timeout(std::time::Duration::from_secs(2),mock.post_started.acquire()).await.unwrap().unwrap().forget();
        signal.cancel(None);let error=tokio::time::timeout(std::time::Duration::from_secs(2),task).await.unwrap().unwrap().unwrap_err();
        assert_eq!(error.message,"Module request was cancelled.");
    }

}
