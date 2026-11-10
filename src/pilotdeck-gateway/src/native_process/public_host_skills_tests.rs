//! Production resolver and skills projection over a real, isolated module HTTP peer.
use super::{assembly, management_tests, public_host::Resolver, sop_control::NativeSopControl};
use crate::{
    metadata::object,
    module_host::{HostBody, PublicHostPrincipal, PublicHostResolver},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use pilotdeck_protocol::JsValue;
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc, time::timeout};

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn json(value: &JsValue) -> serde_json::Value {
    serde_json::from_str(&pilotdeck_protocol::stringify_json(value).unwrap().unwrap()).unwrap()
}
fn principal() -> PublicHostPrincipal {
    PublicHostPrincipal {
        pilot_deck_user_id: "owner".into(),
        tenant_id: "tenant".into(),
        actor_user_id: "actor".into(),
        agent_id: "agent".into(),
    }
}

#[tokio::test]
async fn production_public_skills_external_projection_errors_and_slow_peer_leave_session_io_free() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mode = Arc::new(AtomicU8::new(0));
    let mode_for_server = mode.clone();
    let (sent, mut observed) = mpsc::unbounded_channel();
    let _server = Server(tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (socket, _) = accepted.unwrap();
                    let mode = mode_for_server.clone();
                    let sent = sent.clone();
                    connections.spawn(async move {
                        let service = service_fn(move |request: Request<Incoming>| {
                            let mode = mode.clone(); let sent = sent.clone();
                            async move {
                                let path = request.uri().path().to_owned();
                                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                                let body = if path == "/module-manifest" {
                                    serde_json::json!({"protocolVersion":"2.0","implementationId":"test.skills","contract":"pilotdeck.skills/v1","transport":"module-http-v2","methods":["list"]})
                                } else {
                                    let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                                    sent.send(request.clone()).unwrap();
                                    match mode.load(Ordering::SeqCst) {
                                        1 => return Ok::<_, Infallible>(Response::builder().status(503).body(Full::new(Bytes::from_static(b"{}"))).unwrap()),
                                        2 => std::future::pending::<()>().await,
                                        _ => {}
                                    }
                                    serde_json::json!({"kind":"response","messageId":"reply","inReplyTo":request["messageId"],"requestId":request["requestId"],"ok":true,"payload":{"result":{"items":[{"slug":"external-skill","name":"External","scope":"project"}],"nextCursor":"cursor","total":1}}})
                                };
                                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body.to_string()))))
                            }
                        });
                        let _ = http1::Builder::new().serve_connection(TokioIo::new(socket), service).await;
                    });
                },
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    }));
    let (_scratch, requests) = management_tests::setup();
    let config = requests.options.home.join("pilotdeck.yaml");
    let mut source = std::fs::read_to_string(&config).unwrap();
    source.push_str(&format!("\nmodules:\n  skills:\n    enabled: true\n    implementationId: test.skills\n    contract: pilotdeck.skills/v1\n    transport: module-http-v2\n    endpoint: {endpoint}\n    methods: [list]\n    timeoutMs: 10000\n"));
    std::fs::write(&config, source).unwrap();
    let registry = assembly::project_registry(&requests.options, &requests.env);
    let sop = NativeSopControl::new(
        requests.options.clone(),
        requests.env.clone(),
        registry.clone(),
    )
    .unwrap();
    let resolver = Resolver::new(
        requests.options.clone(),
        requests.env.clone(),
        registry.clone(),
        sop,
    );
    let actor = principal();
    let provider = resolver.resolve(&actor).await.unwrap();
    let reply = timeout(
        Duration::from_secs(3),
        provider.call("list_general_skills", &object([]), &actor),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reply.status, 200);
    let HostBody::Value(body) = reply.body else {
        panic!("skills JSON")
    };
    let body = json(&body);
    assert_eq!(body["data"][0]["id"], "external-skill");
    assert_eq!(body["data"], body["items"]);
    assert_eq!(body["next_cursor"], "cursor");
    assert_eq!(body["nextCursor"], "cursor");
    assert_eq!(body["total"], 1);
    let request = observed.recv().await.unwrap();
    assert_eq!(
        request["payload"]["input"]["projectKey"],
        requests.options.project.to_string_lossy().as_ref()
    );
    mode.store(1, Ordering::SeqCst);
    let reply = timeout(
        Duration::from_secs(3),
        provider.call("list_general_skills", &object([]), &actor),
    )
    .await
    .unwrap();
    let error = match reply {
        Err(error) => error,
        Ok(_) => panic!("HTTP 503 must fail"),
    };
    assert_eq!(error.status, Some(502));
    assert_eq!(error.code.as_deref(), Some("MODULE_TRANSPORT_UNAVAILABLE"));
    observed.recv().await.unwrap();
    mode.store(2, Ordering::SeqCst);
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let provider = provider.clone();
        let actor = actor.clone();
        calls.spawn(async move {
            provider
                .call("list_general_skills", &object([]), &actor)
                .await
        });
    }
    for _ in 0..4 {
        timeout(Duration::from_secs(3), observed.recv())
            .await
            .unwrap()
            .unwrap();
    }
    // All four HTTP replies are still pending. A new real resolver must remain
    // usable, which the former four shared blocking permits prevented.
    let fresh = timeout(Duration::from_secs(2), resolver.resolve(&actor))
        .await
        .expect("slow module HTTP must not monopolize session storage")
        .unwrap();
    drop(fresh);
    calls.abort_all();
    while let Some(result) = calls.join_next().await {
        match result {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("cancelled call completed"),
        }
    }
    for operation in provider.operations() {
        for invalid in [JsValue::Null, JsValue::Array(vec![]), JsValue::Bool(true)] {
            let reply = provider.call(&operation, &invalid, &actor).await.unwrap();
            assert_eq!(reply.status, 400, "{operation} adapter admission");
            let HostBody::Value(body) = reply.body else {
                panic!("invalid JSON")
            };
            assert_eq!(
                json(&body),
                serde_json::json!({"code":"PUBLIC_HOST_INPUT_INVALID","message":"input must be an object."})
            );
        }
    }
    drop(provider);
    drop(resolver);
    registry.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
    requests.project.dispose().await.unwrap();
}
