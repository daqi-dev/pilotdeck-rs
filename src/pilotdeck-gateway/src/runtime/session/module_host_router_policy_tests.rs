//! Trusted organization policy is exercised through the production model host.
use super::auto_route_tests::{callback, host_input, object, request, snapshot};
use super::*;
use crate::host::HostHandler;
use crate::runtime::model::router::RouterGenerationRegistry;
use crate::runtime::sdk_session::{OrganizationPolicy, ProviderPolicy, SelectorPolicy};
use std::sync::Mutex;

fn host(snapshot: JsValue, policy: OrganizationPolicy, source: &str) -> ModuleRouter {
    let generations = RouterGenerationRegistry::default();
    let shared = generations.generation(&"policy-project".into(), Some(policy));
    let mailbox = crate::runtime::steer::SteerMailbox::new(None);
    mailbox.start("turn".into());
    build_with_router_state(
        snapshot,
        &host_input(source),
        Arc::new(EventBuffer::default()),
        crate::runtime::network::Cancellation::default(),
        crate::runtime::steer::TurnSteers::new(mailbox, "turn".into(), vec![]),
        None,
        None,
        None,
        Some(shared),
    )
    .unwrap()
}

#[tokio::test]
async fn production_model_host_rejects_auto_primary_and_provider_origin_before_send() {
    for (policy, expected) in [
        (
            OrganizationPolicy {
                models: Some(SelectorPolicy {
                    deny: vec!["proof/*".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            "GATEWAY_ORGANIZATION_MODEL_DENIED",
        ),
        (
            OrganizationPolicy {
                providers: Some(ProviderPolicy {
                    origins: SelectorPolicy {
                        allow: vec!["https://permitted.invalid".into()],
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
            "GATEWAY_ORGANIZATION_PROVIDER_ORIGIN_DENIED",
        ),
    ] {
        let router = host(snapshot("http://127.0.0.1:1", true), policy, "router");
        let error = router
            .module_call(callback(
                "prepare",
                request("project-proof", "project-only", "auto"),
                "turn",
            ))
            .await
            .unwrap_err();
        assert_eq!(error.module_error.unwrap().code, Some(expected.into()));
    }
}

#[tokio::test]
async fn production_model_host_filters_denied_fallback_without_opening_its_provider() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use tokio::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let calls = recorded.clone();
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |request: Request<Incoming>| {
                            let calls = calls.clone();
                            async move {
                                let path = request.uri().path().to_owned();
                                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                                let body: serde_json::Value =
                                    serde_json::from_slice(&bytes).unwrap();
                                calls.lock().unwrap().push((path, body));
                                Ok::<_, Infallible>(
                                    Response::builder()
                                        .status(503)
                                        .header("connection", "close")
                                        .body(Full::new(Bytes::from_static(
                                            br#"{"error":{"message":"retryable fixture"}}"#,
                                        )))
                                        .unwrap(),
                                )
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    let mut input = host_input("turn");
    let JsValue::Object(fields) = &mut input else {
        panic!("host input")
    };
    fields.insert(
        "fallbackModels".into(),
        JsValue::Array(vec![object([
            ("provider", "proof".into()),
            ("model", "alternate".into()),
        ])]),
    );
    let registry = RouterGenerationRegistry::default();
    let generation = registry.generation(
        &"fallback-project".into(),
        Some(OrganizationPolicy {
            models: Some(SelectorPolicy {
                deny: vec!["proof/*".into()],
                ..Default::default()
            }),
            ..Default::default()
        }),
    );
    let mailbox = crate::runtime::steer::SteerMailbox::new(None);
    mailbox.start("turn".into());
    let router = build_with_router_state(
        snapshot(&base, true),
        &input,
        Arc::new(EventBuffer::default()),
        crate::runtime::network::Cancellation::default(),
        crate::runtime::steer::TurnSteers::new(mailbox, "turn".into(), vec![]),
        None,
        None,
        None,
        Some(generation),
    )
    .unwrap();
    router
        .module_call(callback(
            "prepare",
            request("project-proof", "project-only", "failure"),
            "turn",
        ))
        .await
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        router.module_call(callback(
            "stream",
            request("project-proof", "project-only", "failure"),
            "turn",
        )),
    )
    .await
    .unwrap()
    .unwrap();
    let Some(JsValue::Array(events)) = result.get("events") else {
        panic!("model events")
    };
    assert!(
        events
            .iter()
            .any(|event| event.get("type") == Some(&"error".into()))
    );
    let calls = calls.lock().unwrap();
    assert!(!calls.is_empty(), "allowed primary must be exercised");
    for (path, body) in calls.iter() {
        assert_eq!(
            path, "/project/v1/chat/completions",
            "denied provider must never be opened"
        );
        assert_eq!(
            body["model"], "project-only",
            "denied fallback must never be sent"
        );
    }
    server.abort();
}
