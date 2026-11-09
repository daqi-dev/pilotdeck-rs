//! Native programmatic router factories reach production prepare/HTTP stream.
use super::auto_route_tests::{callback, host_input, object, request, snapshot};
use super::*;
use crate::host::HostHandler;
use crate::runtime::model::router::{
    CustomRouter, CustomRouterInstance, CustomRouterRegistry, RouterFuture,
    RouterGenerationRegistry,
};
use crate::runtime::plugins::{
    LoadedPlugin, PluginRuntime, PluginRuntimeOptions, PluginSource, RouterContribution,
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct RetainedRouter {
    session: &'static str,
    provider: &'static str,
    model: &'static str,
}
impl CustomRouter for RetainedRouter {
    fn decide(&self, input: JsValue) -> RouterFuture<JsValue> {
        assert_eq!(
            input
                .get("context")
                .and_then(|context| context.get("sessionId")),
            Some(&self.session.into())
        );
        let decision = object([
            ("provider", self.provider.into()),
            ("model", self.model.into()),
        ]);
        Box::pin(async move { Ok(decision) })
    }
}

fn host(snapshot: JsValue, session: &str, generation: Arc<RouterSharedState>) -> ModuleRouter {
    let mut input = host_input("router");
    let JsValue::Object(fields) = &mut input else {
        unreachable!()
    };
    fields.insert("sessionId".into(), session.into());
    let mailbox = crate::runtime::steer::SteerMailbox::new(None);
    mailbox.start("turn".into());
    build_with_router_state(
        snapshot,
        &input,
        Arc::new(EventBuffer::default()),
        crate::runtime::network::Cancellation::default(),
        crate::runtime::steer::TurnSteers::new(mailbox, "turn".into(), vec![]),
        None,
        None,
        None,
        Some(generation),
    )
    .unwrap()
}

#[tokio::test]
async fn retained_programmatic_factories_route_two_production_hosts_and_dispose_independently() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Response, server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let captured = captured.clone();
            tokio::spawn(async move {
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let captured = captured.clone();
                    async move {
                        let path = request.uri().path().to_owned();
                        let body = request.into_body().collect().await.unwrap().to_bytes();
                        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        captured.lock().unwrap().push((path, body));
                        Ok::<_, Infallible>(Response::builder()
                            .header("content-type", "text/event-stream")
                            .header("connection", "close")
                            .body(Full::new(Bytes::from_static(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"routed\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))).unwrap())
                    }
                })).await;
            });
        }
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let runtime = PluginRuntime::new(PluginRuntimeOptions {
        project_root: "/missing/production-router-project".into(),
        pilot_home: "/missing/production-router-home".into(),
        builtin_skills_root: None,
        builtin_plugins_enabled: Default::default(),
        builtin_plugins: vec![Arc::new(LoadedPlugin {
            name: "native-router-plugin".into(),
            path: "/missing/native-router-plugin".into(),
            source: PluginSource::Builtin,
            commands: vec![],
            skills: vec![],
            output_styles: vec![],
            hooks_config: None,
            mcp_servers: None,
            lsp_servers: None,
            dispose: None,
            router_contributions: vec![RouterContribution {
                id: "selected-router".into(),
                description: None,
                create_custom_router: Arc::new(move || {
                    let first = count.fetch_add(1, Ordering::SeqCst) == 0;
                    Ok(CustomRouterInstance::new(
                        "selected-router",
                        Arc::new(RetainedRouter {
                            session: if first { "first" } else { "second" },
                            provider: if first { "proof" } else { "project-proof" },
                            model: if first { "alternate" } else { "project-only" },
                        }),
                    ))
                }),
            }],
        })],
    });
    runtime.refresh().await.unwrap();
    let mut first_lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
    let mut second_lease = runtime.acquire_session_contributions(vec![]).await.unwrap();
    let generations = RouterGenerationRegistry::default();
    let first = generations.generation(&"project".into(), None);
    let second = generations.generation(&"project".into(), None);
    first
        .custom_routers
        .register_session(
            "first".into(),
            first_lease.contributions.create_custom_routers().unwrap(),
        )
        .unwrap();
    second
        .custom_routers
        .register_session(
            "second".into(),
            second_lease.contributions.create_custom_routers().unwrap(),
        )
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    let mut root = snapshot(&base_url, false).as_object().unwrap().clone();
    let mut config = root.get("config").unwrap().as_object().unwrap().clone();
    let mut router = config.get("router").unwrap().as_object().unwrap().clone();
    router.insert(
        "customRouter".into(),
        object([("extensionId", "selected-router".into())]),
    );
    config.insert("router".into(), JsValue::Object(router));
    root.insert("config".into(), JsValue::Object(config));
    let loaded = JsValue::Object(root);
    let first_host = host(loaded.clone(), "first", first.clone());
    let second_host = host(loaded, "second", second.clone());
    let prepare = |host: &ModuleRouter| {
        let future = host.module_call(callback(
            "prepare",
            request("project-proof", "project-only", "hello"),
            "turn",
        ));
        async move { future.await.unwrap() }
    };
    let prepared = prepare(&first_host).await;
    assert_eq!(
        prepared
            .get("prepared")
            .and_then(|value| value.get("provider")),
        Some(&"proof".into())
    );
    let prepared = prepare(&second_host).await;
    assert_eq!(
        prepared
            .get("prepared")
            .and_then(|value| value.get("provider")),
        Some(&"project-proof".into())
    );
    let stream = first_host
        .module_call(callback(
            "stream",
            request("project-proof", "project-only", "first"),
            "turn",
        ))
        .await
        .unwrap();
    let Some(JsValue::Array(events)) = stream.get("events") else {
        panic!("stream events")
    };
    assert!(
        events
            .iter()
            .any(|event| event.get("type") == Some(&"text_delta".into()))
    );
    first.dispose();
    assert!(
        first
            .custom_routers
            .lookup_router(&"selected-router".into(), &"first".into())
            .is_none()
    );
    assert!(
        second
            .custom_routers
            .lookup_router(&"selected-router".into(), &"second".into())
            .is_some()
    );
    let stream = second_host
        .module_call(callback(
            "stream",
            request("proof", "alternate", "second"),
            "turn",
        ))
        .await
        .unwrap();
    let Some(JsValue::Array(events)) = stream.get("events") else {
        panic!("stream events")
    };
    assert!(
        events
            .iter()
            .any(|event| event.get("type") == Some(&"text_delta".into()))
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        (&*requests[0].0, &requests[0].1["model"]),
        ("/auto/v1/chat/completions", &serde_json::json!("alternate"))
    );
    assert_eq!(
        (&*requests[1].0, &requests[1].1["model"]),
        (
            "/project/v1/chat/completions",
            &serde_json::json!("project-only")
        )
    );
    drop(requests);
    second.dispose();
    first_lease.release().await.unwrap();
    second_lease.release().await.unwrap();
    runtime.dispose().await.unwrap();
    server.abort();
}
