//! Exercise the production HTTP upgrade/WS path with an injected pending bus.
use crate::runtime::interaction::{GatewayInteractionCoordinator, PendingEntry};
use crate::runtime::permission::{
    DefaultPermissionContextOptions, PermissionTool, create_default_permission_context, decide,
};
use crate::{LocalServer, dispatch, frame, http_proxy::Proxy, metadata::object};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    time::{Duration, timeout},
};

async fn next(stream: &mut DuplexStream, decoder: &mut frame::Decoder) -> frame::Frame {
    timeout(Duration::from_secs(3), async {
        loop {
            if let Some(frame) = decoder.next_server().unwrap() {
                return frame;
            }
            assert_ne!(stream.read_buf(&mut decoder.buffer).await.unwrap(), 0);
        }
    })
    .await
    .expect("WS response timeout")
}

async fn send(stream: &mut DuplexStream, value: JsValue) {
    let json = stringify_json(&value).unwrap().unwrap();
    let length = if json.len() < 126 {
        json.len() as u8
    } else {
        126
    };
    stream
        .write_all(&frame::client_frame(0x81, length, json.as_bytes()))
        .await
        .unwrap();
}

async fn open(
    server: Arc<LocalServer>,
    token: &str,
    client_name: &str,
) -> (
    DuplexStream,
    frame::Decoder,
    tokio::task::JoinHandle<Result<(), hyper::Error>>,
) {
    let (stream, mut client) = tokio::io::duplex(16384);
    let task = tokio::spawn(server.serve_connection(stream));
    client.write_all(b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").await.unwrap();
    let mut headers = Vec::new();
    timeout(Duration::from_secs(3), async {
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(client.read_u8().await.unwrap());
        }
    })
    .await
    .expect("HTTP upgrade timeout");
    assert!(headers.starts_with(b"HTTP/1.1 101 "));
    send(
        &mut client,
        object([
            ("type", "hello".into()),
            ("protocolVersion", "1.2".into()),
            ("clientName", client_name.into()),
            ("clientVersion", "1".into()),
            ("token", token.into()),
        ]),
    )
    .await;
    (client, frame::Decoder::default(), task)
}

fn request(params: JsValue) -> JsValue {
    object([
        ("type", "request".into()),
        ("id", "perm".into()),
        ("method", "permission_list".into()),
        ("params", params),
    ])
}

#[tokio::test]
async fn user_dialog_list_offline_uses_injected_channel_state_across_connections() {
    use crate::user_dialog::{
        ChannelOptions, GatewayUserDialogChannel, GatewayUserDialogs, InputRequest,
    };
    let dialogs = Arc::new(GatewayUserDialogs::default());
    let mut options = ChannelOptions::new("s", dialogs.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "dialog".into());
    let channel = GatewayUserDialogChannel::new(options);
    let pending = channel.request_input(&InputRequest {
        tool_call_id: "c".into(),
        tool_name: "input".into(),
        prompt: "真实 pending".into(),
        placeholder: None,
        allow_empty: false,
        abort: None,
    });
    let token = uuid::Uuid::new_v4().to_string();
    let server = Arc::new(
        {
            let mut server = LocalServer::with_token(
                Arc::new(Proxy {
                    upstream: "http://127.0.0.1:0".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                }),
                token.clone(),
            )
            .unwrap();
            server.interactions = Arc::default();
            server.dialogs = Arc::default();
            server
        }
        .with_user_dialogs(dialogs.clone()),
    );
    let (mut first, mut fd, ft) = open(server.clone(), &token, "cli").await;
    assert_eq!(
        dispatch::parse(&next(&mut first, &mut fd).await.payload)
            .unwrap()
            .get("type"),
        Some(&"hello_ok".into())
    );
    let (mut second, mut sd, st) = open(server, &token, "sdk").await;
    next(&mut second, &mut sd).await;
    let request = object([
        ("type", "request".into()),
        ("id", "list".into()),
        ("method", "user_dialog_list".into()),
        ("params", object([("sessionKey", "s".into())])),
    ]);
    for (client, decoder) in [(&mut first, &mut fd), (&mut second, &mut sd)] {
        send(client, request.clone()).await;
        let reply = dispatch::parse(&next(client, decoder).await.payload).unwrap();
        assert_eq!(reply.get("ok"), Some(&JsValue::Bool(true)));
        let JsValue::Array(events) = reply.get("result").unwrap().get("dialogs").unwrap() else {
            panic!("dialogs")
        };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].get("requestId"), Some(&"dialog".into()));
        assert!(pending.is_pending());
    }
    channel.abort(pending.request_id());
    send(&mut second, request).await;
    let reply = dispatch::parse(&next(&mut second, &mut sd).await.payload).unwrap();
    assert_eq!(
        reply.get("result"),
        Some(&object([("dialogs", JsValue::Array(vec![]))]))
    );
    send(
        &mut first,
        object([
            ("type", "request".into()),
            ("id", "bad".into()),
            ("method", "user_dialog_list".into()),
            ("params", object([("sessionKey", " ".into())])),
        ]),
    )
    .await;
    let reply = dispatch::parse(&next(&mut first, &mut fd).await.payload).unwrap();
    assert_eq!(
        reply.get("error"),
        Some(&object([
            ("code", "INVALID_USER_DIALOG_LIST".into()),
            ("message", "sessionKey is required.".into())
        ]))
    );
    for (mut client, mut decoder, task) in [(first, fd, ft), (second, sd, st)] {
        client
            .write_all(&frame::client_frame(0x88, 0, b""))
            .await
            .unwrap();
        assert_eq!(next(&mut client, &mut decoder).await.opcode, 8);
        drop(client);
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn elicitation_respond_offline_binding_is_mandatory_even_for_sdk_control() {
    use crate::runtime::interaction::InteractionBinding;
    for name in ["cli", "sdk"] {
        let interactions = Arc::new(GatewayInteractionCoordinator::default());
        let token = uuid::Uuid::new_v4().to_string();
        let server = Arc::new({
            let mut server = LocalServer::with_token(
                Arc::new(Proxy {
                    upstream: "http://127.0.0.1:0".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                }),
                token.clone(),
            )
            .unwrap();
            server.dialogs = Arc::default();
            server.interactions = interactions.clone();
            server
        });
        let (mut old, mut old_decoder, old_task) = open(server.clone(), &token, name).await;
        let hello = dispatch::parse(&next(&mut old, &mut old_decoder).await.payload).unwrap();
        let first =
            InteractionBinding::from_value(hello.get("interactionBinding").unwrap()).unwrap();
        interactions
            .reconnect_for_turn(&"s".into(), first.clone())
            .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Mutex::new(Some(tx));
        let reg = interactions
            .elicitation_bus()
            .register(
                &"s".into(),
                PendingEntry {
                    request_id: "q".into(),
                    tool_call_id: "c".into(),
                    tool_name: "askUser".into(),
                    resolve: Arc::new(move |answer| {
                        tx.lock().unwrap().take().unwrap().send(answer).unwrap()
                    }),
                    reject: Arc::new(|_| panic!("response should not reject")),
                },
                None,
            )
            .unwrap();
        let (mut current, mut current_decoder, current_task) =
            open(server.clone(), &token, name).await;
        let hello =
            dispatch::parse(&next(&mut current, &mut current_decoder).await.payload).unwrap();
        let second =
            InteractionBinding::from_value(hello.get("interactionBinding").unwrap()).unwrap();
        interactions
            .reconnect(&"s".into(), Some(second.clone()), Some(&first))
            .unwrap();
        let answer = JsValue::Array(vec!["原样".into(), JsValue::Null]);
        // Neither socket submits a turn: the SDK control exception must not apply.
        for (use_current, supplied, expected) in [
            (false, Some(second.to_value()), false),
            (false, None, false),
            (true, Some(first.to_value()), true),
            (true, None, false),
        ] {
            let (client, decoder) = if use_current {
                (&mut current, &mut current_decoder)
            } else {
                (&mut old, &mut old_decoder)
            };
            let mut params = object([
                ("sessionKey", "s".into()),
                ("requestId", "q".into()),
                ("answer", answer.clone()),
            ]);
            if let (JsValue::Object(params), Some(binding)) = (&mut params, supplied) {
                params.insert("interactionBinding".into(), binding);
            }
            send(
                client,
                object([
                    ("type", "request".into()),
                    ("id", "q".into()),
                    ("method", "elicitation_respond".into()),
                    ("params", params),
                ]),
            )
            .await;
            let reply = dispatch::parse(&next(client, decoder).await.payload).unwrap();
            assert_eq!(
                reply,
                object([
                    ("type", "response".into()),
                    ("id", "q".into()),
                    ("ok", JsValue::Bool(true)),
                    ("result", object([("delivered", JsValue::Bool(expected))]))
                ])
            );
        }
        assert_eq!(
            timeout(Duration::from_secs(1), rx).await.unwrap().unwrap(),
            answer
        );
        assert!(!reg.active());
        assert_eq!(interactions.elicitation_bus().pending_count(&"s".into()), 0);
        // WS null/undefined are spread into an object, without permission's post-dispatch error.
        for params in [None, Some(JsValue::Null), Some(object([]))] {
            let mut request = object([
                ("type", "request".into()),
                ("id", "missing".into()),
                ("method", "elicitation_respond".into()),
            ]);
            if let (JsValue::Object(request), Some(params)) = (&mut request, params) {
                request.insert("params".into(), params);
            }
            send(&mut current, request).await;
            let reply =
                dispatch::parse(&next(&mut current, &mut current_decoder).await.payload).unwrap();
            assert_eq!(
                reply.get("result"),
                Some(&object([("delivered", JsValue::Bool(false))]))
            );
        }
        for (mut client, mut decoder, task) in [
            (old, old_decoder, old_task),
            (current, current_decoder, current_task),
        ] {
            client
                .write_all(&frame::client_frame(0x88, 0, b""))
                .await
                .unwrap();
            assert_eq!(next(&mut client, &mut decoder).await.opcode, 8);
            drop(client);
            timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
}

#[tokio::test]
async fn permission_decide_offline_uses_authenticated_binding_and_sdk_control_exception() {
    use crate::runtime::interaction::InteractionBinding;
    for name in ["cli", "sdk"] {
        let interactions = Arc::new(GatewayInteractionCoordinator::default());
        let token = uuid::Uuid::new_v4().to_string();
        let server = Arc::new({
            let mut server = LocalServer::with_token(
                Arc::new(Proxy {
                    upstream: "http://127.0.0.1:0".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                }),
                token.clone(),
            )
            .unwrap();
            server.dialogs = Arc::default();
            server.interactions = interactions.clone();
            server
        });
        let (mut old, mut old_decoder, old_task) = open(server.clone(), &token, name).await;
        let first = dispatch::parse(&next(&mut old, &mut old_decoder).await.payload).unwrap();
        let first =
            InteractionBinding::from_value(first.get("interactionBinding").unwrap()).unwrap();
        interactions
            .reconnect_for_turn(&"s".into(), first.clone())
            .unwrap();
        // SDK run membership is recorded before a submit succeeds, as in TS.
        send(
            &mut old,
            object([
                ("type", "request".into()),
                ("id", "run".into()),
                ("method", "submit_turn".into()),
                ("params", object([("sessionKey", "s".into())])),
            ]),
        )
        .await;
        assert_eq!(
            dispatch::parse(&next(&mut old, &mut old_decoder).await.payload)
                .unwrap()
                .get("ok"),
            Some(&JsValue::Bool(false))
        );
        let answer = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received = answer.clone();
        let registration = interactions
            .permission_bus()
            .register(
                &"s".into(),
                PendingEntry {
                    request_id: "p".into(),
                    tool_call_id: "c".into(),
                    tool_name: "bash".into(),
                    resolve: Arc::new(move |value| received.lock().unwrap().push(value)),
                    reject: Arc::new(|_| {}),
                },
                None,
            )
            .unwrap();
        let (mut current, mut current_decoder, current_task) =
            open(server.clone(), &token, name).await;
        let second =
            dispatch::parse(&next(&mut current, &mut current_decoder).await.payload).unwrap();
        let second =
            InteractionBinding::from_value(second.get("interactionBinding").unwrap()).unwrap();
        interactions
            .reconnect(&"s".into(), Some(second.clone()), Some(&first))
            .unwrap();
        for (client, decoder, forged, expected) in [
            (&mut old, &mut old_decoder, second.to_value(), false),
            (&mut current, &mut current_decoder, first.to_value(), true),
        ] {
            // Mark both SDK sockets as run owners without modifying turn code.
            send(
                client,
                object([
                    ("type", "request".into()),
                    ("id", "run".into()),
                    ("method", "submit_turn".into()),
                    ("params", object([("sessionKey", "s".into())])),
                ]),
            )
            .await;
            next(client, decoder).await;
            send(
                client,
                object([
                    ("type", "request".into()),
                    ("id", "answer".into()),
                    ("method", "permission_decide".into()),
                    (
                        "params",
                        object([
                            ("sessionKey", "s".into()),
                            ("requestId", "p".into()),
                            ("projectKey", "project".into()),
                            ("decision", "allow".into()),
                            ("remember", JsValue::Bool(true)),
                            ("interactionBinding", forged),
                        ]),
                    ),
                ]),
            )
            .await;
            let notice = dispatch::parse(&next(client, decoder).await.payload).unwrap();
            assert_eq!(
                notice,
                object([
                    ("type", "notification".into()),
                    ("name", "permission_changed".into()),
                    (
                        "payload",
                        object([
                            ("type", "resolved".into()),
                            ("sessionKey", "s".into()),
                            ("projectKey", "project".into()),
                            ("requestId", "p".into())
                        ])
                    )
                ])
            );
            let reply = dispatch::parse(&next(client, decoder).await.payload).unwrap();
            assert_eq!(
                reply.get("result"),
                Some(&object([("delivered", JsValue::Bool(expected))]))
            );
            assert_eq!(registration.active(), !expected);
        }
        assert_eq!(answer.lock().unwrap().len(), 1);
        assert_eq!(
            answer.lock().unwrap()[0].get("remember"),
            Some(&JsValue::Bool(true))
        );
        let received = answer.clone();
        let control_reg = interactions
            .permission_bus()
            .register(
                &"s".into(),
                PendingEntry {
                    request_id: "control".into(),
                    tool_call_id: "c".into(),
                    tool_name: "bash".into(),
                    resolve: Arc::new(move |value| received.lock().unwrap().push(value)),
                    reject: Arc::new(|_| {}),
                },
                None,
            )
            .unwrap();
        let (mut control, mut control_decoder, control_task) = open(server, &token, name).await;
        next(&mut control, &mut control_decoder).await;
        for supplied in [Some(first.to_value()), None] {
            let mut params = object([
                ("sessionKey", "s".into()),
                ("requestId", "control".into()),
                ("decision", "deny".into()),
            ]);
            if let (JsValue::Object(fields), Some(binding)) = (&mut params, &supplied) {
                fields.insert("interactionBinding".into(), binding.clone());
            }
            send(
                &mut control,
                object([
                    ("type", "request".into()),
                    ("id", "control".into()),
                    ("method", "permission_decide".into()),
                    ("params", params),
                ]),
            )
            .await;
            next(&mut control, &mut control_decoder).await; // notification even for a miss
            let reply =
                dispatch::parse(&next(&mut control, &mut control_decoder).await.payload).unwrap();
            let expected = name == "sdk" && supplied.is_none();
            assert_eq!(
                reply.get("result"),
                Some(&object([("delivered", JsValue::Bool(expected))]))
            );
            assert_eq!(control_reg.active(), !expected);
        }
        assert_eq!(
            answer.lock().unwrap().len(),
            if name == "sdk" { 2 } else { 1 }
        );
        control_reg.dispose();
        for (mut client, mut decoder, task) in [
            (old, old_decoder, old_task),
            (current, current_decoder, current_task),
            (control, control_decoder, control_task),
        ] {
            client
                .write_all(&frame::client_frame(0x88, 0, b""))
                .await
                .unwrap();
            assert_eq!(next(&mut client, &mut decoder).await.opcode, 8);
            drop(client);
            timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }
}

#[tokio::test]
async fn grant_session_permission_ws_mutates_host_rules_and_changes_permission_decision() {
    use crate::config::permission::GatewaySessionPermissionRuleSetRegistry;
    use crate::runtime::interaction::InteractionOptions;
    struct WriteTool(JsString);
    impl PermissionTool for WriteTool {
        fn name(&self) -> &JsString {
            &self.0
        }
        fn is_read_only(&self, _: &JsValue) -> Result<JsValue, crate::host::HostError> {
            Ok(JsValue::Bool(false))
        }
    }
    let registry = Arc::new(GatewaySessionPermissionRuleSetRegistry::new());
    let interactions = Arc::new(GatewayInteractionCoordinator::new(InteractionOptions {
        permission_grants: Some(registry.clone()),
        ..Default::default()
    }));
    let token = uuid::Uuid::new_v4().to_string();
    let server = Arc::new(
        {
            let mut server = LocalServer::with_token(
                Arc::new(Proxy {
                    upstream: "http://127.0.0.1:0".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                }),
                token.clone(),
            )
            .unwrap();
            server.dialogs = Arc::default();
            server.interactions = Arc::default();
            server
        }
        .with_interactions(interactions.clone()),
    );
    let owner = JsString::from("host-session");
    let tool = WriteTool("write_file".into());
    let input = object([("file_path", "note.txt".into())]);
    let context = |rules| {
        create_default_permission_context(DefaultPermissionContextOptions {
            cwd: "/ws".into(),
            can_prompt: Some(true),
            rules,
            ..Default::default()
        })
    };
    let before = decide(&context(Default::default()), &tool, &input, &"call".into())
        .await
        .unwrap();
    assert_ne!(before.get("type"), Some(&"allow".into()));
    let mut lease = None;
    for (index, entry) in [" Write ", "write_file", "Read"].iter().enumerate() {
        let (mut client, mut decoder, task) = open(server.clone(), &token, "perm-test").await;
        assert_eq!(next(&mut client, &mut decoder).await.opcode, 1);
        send(
            &mut client,
            object([
                ("type", "request".into()),
                ("id", "grant".into()),
                ("method", "grant_session_permission".into()),
                (
                    "params",
                    object([
                        ("sessionKey", JsValue::String(owner.clone())),
                        ("entry", (*entry).into()),
                        (
                            "interactionBinding",
                            object([("connectionId", "irrelevant-to-grant".into())]),
                        ),
                    ]),
                ),
            ]),
        )
        .await;
        let response = dispatch::parse(&next(&mut client, &mut decoder).await.payload).unwrap();
        assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
        assert_eq!(
            response.get("result"),
            Some(&object([
                ("granted", JsValue::Bool(true)),
                ("entry", (*entry).into())
            ]))
        );
        if index == 0 {
            lease = Some(registry.acquire(&owner, None));
        }
        let rules = lease.as_ref().unwrap().rules().snapshot();
        assert_eq!(rules.allow.len(), if index < 2 { 1 } else { 2 });
        let decision = decide(&context(rules), &tool, &input, &"call".into())
            .await
            .unwrap();
        assert_eq!(decision.get("type"), Some(&"allow".into()));
        assert_eq!(
            decision.get("reason").unwrap().get("message"),
            Some(&"Session allow rule permits write_file.".into())
        );
        client
            .write_all(&frame::client_frame(0x88, 0, b""))
            .await
            .unwrap();
        assert_eq!(next(&mut client, &mut decoder).await.opcode, 8);
        drop(client);
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        registry.size(),
        1,
        "disconnect does not revoke a session grant"
    );
    let (mut client, mut decoder, task) = open(server, "wrong-isolated-token", "perm-test").await;
    send(
        &mut client,
        object([
            ("type", "request".into()),
            ("id", "grant".into()),
            ("method", "grant_session_permission".into()),
            (
                "params",
                object([
                    ("sessionKey", "unauthorized".into()),
                    ("entry", "Bash".into()),
                ]),
            ),
        ]),
    )
    .await;
    let close = next(&mut client, &mut decoder).await;
    assert_eq!(close.opcode, 8);
    assert_eq!(
        u16::from_be_bytes(close.payload[..2].try_into().unwrap()),
        4003
    );
    drop(client);
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(registry.allow_rules(&"unauthorized".into()).is_empty());
    interactions.close_session(&owner, &"closed".into());
    assert_eq!(
        registry.size(),
        1,
        "the active host lease still retains the rules"
    );
    lease.unwrap().release();
    assert_eq!(registry.size(), 0);
    let next = registry.acquire(&owner, None);
    let decision = decide(
        &context(next.rules().snapshot()),
        &tool,
        &input,
        &"call".into(),
    )
    .await
    .unwrap();
    assert_ne!(decision.get("type"), Some(&"allow".into()));
    next.release();
}

#[tokio::test]
async fn permission_list_http_upgrade_shares_host_pending_across_authenticated_connections() {
    let interactions = Arc::new(GatewayInteractionCoordinator::default());
    let registration = interactions
        .permission_bus()
        .register(
            &"session".into(),
            PendingEntry {
                request_id: "approval".into(),
                tool_call_id: "call".into(),
                tool_name: "write_file".into(),
                resolve: Arc::new(|_| panic!("query must not settle")),
                reject: Arc::new(|_| {}),
            },
            Some(object([("path", "report.md".into())])),
        )
        .unwrap();
    let token = uuid::Uuid::new_v4().to_string();
    let server = Arc::new(
        {
            let mut server = LocalServer::with_token(
                Arc::new(Proxy {
                    upstream: "http://127.0.0.1:0".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                }),
                token.clone(),
            )
            .unwrap();
            server.dialogs = Arc::default();
            server.interactions = Arc::default();
            server
        }
        .with_interactions(interactions),
    );
    for index in 0..2 {
        let (mut client, mut decoder, task) = open(server.clone(), &token, "perm-test").await;
        let hello = next(&mut client, &mut decoder).await;
        assert_eq!(
            dispatch::parse(&hello.payload).unwrap().get("type"),
            Some(&"hello_ok".into())
        );
        // Caller-provided owner/binding cannot select another coordinator.
        send(
            &mut client,
            request(object([
                ("sessionKey", "session".into()),
                (
                    "interactionBinding",
                    object([
                        ("connectionId", "forged".into()),
                        ("generation", JsValue::Number(99.0)),
                    ]),
                ),
            ])),
        )
        .await;
        let response = next(&mut client, &mut decoder).await;
        assert_eq!(
            dispatch::parse(&response.payload).unwrap(),
            object([
                ("type", "response".into()),
                ("id", "perm".into()),
                ("ok", JsValue::Bool(true)),
                (
                    "result",
                    object([(
                        "requests",
                        JsValue::Array(vec![object([
                            ("requestId", "approval".into()),
                            ("toolCallId", "call".into()),
                            ("toolName", "write_file".into()),
                            ("payload", object([("path", "report.md".into())])),
                        ])])
                    )])
                ),
            ])
        );
        assert!(registration.active());
        if index == 1 {
            registration.dispose();
            send(
                &mut client,
                request(object([("sessionKey", "session".into())])),
            )
            .await;
            let response = next(&mut client, &mut decoder).await;
            assert_eq!(
                dispatch::parse(&response.payload).unwrap().get("result"),
                Some(&object([("requests", JsValue::Array(vec![]))]))
            );
        }
        send(&mut client, request(object([]))).await;
        let response = dispatch::parse(&next(&mut client, &mut decoder).await.payload).unwrap();
        assert_eq!(
            response.get("error").unwrap().get("code"),
            Some(&"INVALID_PERMISSION_LIST".into())
        );
        client
            .write_all(&frame::client_frame(0x88, 0, b""))
            .await
            .unwrap();
        assert_eq!(next(&mut client, &mut decoder).await.opcode, 8);
        drop(client);
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    let (mut client, mut decoder, task) =
        open(server, "incorrect-isolated-token", "perm-test").await;
    send(
        &mut client,
        request(object([("sessionKey", "session".into())])),
    )
    .await;
    let close = next(&mut client, &mut decoder).await;
    assert_eq!(close.opcode, 8);
    assert_eq!(
        u16::from_be_bytes(close.payload[..2].try_into().unwrap()),
        4003
    );
    assert!(
        decoder.next_server().unwrap().is_none(),
        "unauthenticated query leaked output"
    );
    drop(client);
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn user_dialog_claim_offline_two_connections_share_bearer_lease_and_pending_channel() {
    use crate::user_dialog::{
        ChannelOptions, GatewayUserDialogChannel, GatewayUserDialogs, InputRequest,
    };
    let dialogs = Arc::new(GatewayUserDialogs::default());
    let mut options = ChannelOptions::new("s", dialogs.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "d".into());
    let channel = GatewayUserDialogChannel::new(options);
    let pending = channel.request_input(&InputRequest {
        tool_call_id: "c".into(),
        tool_name: "input".into(),
        prompt: "pending".into(),
        placeholder: None,
        allow_empty: false,
        abort: None,
    });
    let token = uuid::Uuid::new_v4().to_string();
    let server = Arc::new(
        {
            let mut server = LocalServer::with_token(
                Arc::new(Proxy {
                    upstream: "http://127.0.0.1:0".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                }),
                token.clone(),
            )
            .unwrap();
            server.interactions = Arc::default();
            server.dialogs = Arc::default();
            server
        }
        .with_user_dialogs(dialogs),
    );
    let (mut cli, mut cd, ct) = open(server.clone(), &token, "cli").await;
    next(&mut cli, &mut cd).await;
    let (mut sdk, mut sd, st) = open(server, &token, "sdk").await;
    next(&mut sdk, &mut sd).await;
    let mut params = object([
        ("sessionKey", "s".into()),
        ("requestId", "d".into()),
        ("ttlMs", JsValue::Number(1000.0)),
        ("interactionBinding", "caller-controlled".into()),
    ]);
    let request = |params| {
        object([
            ("type", "request".into()),
            ("id", "claim".into()),
            ("method", "user_dialog_claim".into()),
            ("params", params),
        ])
    };
    send(&mut cli, request(params.clone())).await;
    send(&mut sdk, request(params.clone())).await;
    let first = dispatch::parse(&next(&mut cli, &mut cd).await.payload).unwrap();
    let second = dispatch::parse(&next(&mut sdk, &mut sd).await.payload).unwrap();
    assert_eq!(first.get("ok"), Some(&JsValue::Bool(true)));
    assert_eq!(second.get("ok"), Some(&JsValue::Bool(true)));
    let a = first.get("result").unwrap();
    let b = second.get("result").unwrap();
    let (winner, loser) = if a.get("claimed") == Some(&JsValue::Bool(true)) {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(loser.get("reason"), Some(&"claimed".into()));
    let lease = winner.get("leaseId").unwrap().clone();
    set_claim_lease(&mut params, lease.clone());
    send(&mut sdk, request(params.clone())).await; // bearer works on another socket/SDK
    let reply = dispatch::parse(&next(&mut sdk, &mut sd).await.payload).unwrap();
    assert_eq!(reply.get("result").unwrap().get("leaseId"), Some(&lease));
    set_claim_lease(&mut params, "wrong".into());
    send(&mut cli, request(params.clone())).await;
    let reply = dispatch::parse(&next(&mut cli, &mut cd).await.payload).unwrap();
    assert_eq!(
        reply.get("result").unwrap().get("reason"),
        Some(&"claimed".into())
    );
    if let JsValue::Object(params) = &mut params {
        params.insert("ttlMs".into(), JsValue::Number(999.0));
    }
    send(&mut cli, request(params)).await;
    let reply = dispatch::parse(&next(&mut cli, &mut cd).await.payload).unwrap();
    assert_eq!(
        reply.get("error").unwrap().get("code"),
        Some(&"INVALID_USER_DIALOG_CLAIM".into())
    );
    assert!(pending.is_pending());
    channel.abort(pending.request_id());
    for (mut client, mut decoder, task) in [(cli, cd, ct), (sdk, sd, st)] {
        client
            .write_all(&frame::client_frame(0x88, 0, b""))
            .await
            .unwrap();
        assert_eq!(next(&mut client, &mut decoder).await.opcode, 8);
        drop(client);
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

fn set_claim_lease(params: &mut JsValue, lease: JsValue) {
    if let JsValue::Object(params) = params {
        params.insert("leaseId".into(), lease);
    }
}

#[tokio::test]
async fn user_dialog_release_offline_cross_connection_bearer_keeps_pending() {
    use crate::user_dialog::{
        ChannelOptions, GatewayUserDialogChannel, GatewayUserDialogs, InputRequest,
    };
    let dialogs = Arc::new(GatewayUserDialogs::default());
    let mut options = ChannelOptions::new("s", dialogs.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "d".into());
    let channel = GatewayUserDialogChannel::new(options);
    let pending = channel.request_input(&InputRequest {
        tool_call_id: "c".into(),
        tool_name: "input".into(),
        prompt: "pending".into(),
        placeholder: None,
        allow_empty: false,
        abort: None,
    });
    let token = uuid::Uuid::new_v4().to_string();
    let server = Arc::new(
        {
            let mut server = LocalServer::with_token(
                Arc::new(Proxy {
                    upstream: "http://127.0.0.1:0".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                }),
                token.clone(),
            )
            .unwrap();
            server.interactions = Arc::default();
            server.dialogs = Arc::default();
            server
        }
        .with_user_dialogs(dialogs),
    );
    let (mut cli, mut cd, ct) = open(server.clone(), &token, "cli").await;
    next(&mut cli, &mut cd).await;
    let (mut sdk, mut sd, st) = open(server, &token, "sdk").await;
    next(&mut sdk, &mut sd).await;
    let params = object([
        ("sessionKey", "s".into()),
        ("requestId", "d".into()),
        ("ttlMs", JsValue::Number(1000.0)),
        ("interactionBinding", "caller-controlled".into()),
    ]);
    let request = |params| {
        object([
            ("type", "request".into()),
            ("id", "claim".into()),
            ("method", "user_dialog_claim".into()),
            ("params", params),
        ])
    };
    send(&mut cli, request(params.clone())).await;
    send(&mut sdk, request(params.clone())).await;
    let first = dispatch::parse(&next(&mut cli, &mut cd).await.payload).unwrap();
    let second = dispatch::parse(&next(&mut sdk, &mut sd).await.payload).unwrap();
    assert_eq!(first.get("ok"), Some(&JsValue::Bool(true)));
    assert_eq!(second.get("ok"), Some(&JsValue::Bool(true)));
    let a = first.get("result").unwrap();
    let b = second.get("result").unwrap();
    let (winner, loser) = if a.get("claimed") == Some(&JsValue::Bool(true)) {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(loser.get("reason"), Some(&"claimed".into()));
    let lease = winner.get("leaseId").unwrap().clone();
    let release = |lease| {
        object([
            ("type", "request".into()),
            ("id", "release".into()),
            ("method", "user_dialog_release".into()),
            (
                "params",
                object([
                    ("sessionKey", "s".into()),
                    ("requestId", "d".into()),
                    ("leaseId", lease),
                    ("interactionBinding", "raw".into()),
                ]),
            ),
        ])
    };
    for (bearer, released) in [
        ("wrong".into(), false),
        (lease.clone(), true),
        (lease, false),
    ] {
        send(&mut sdk, release(bearer)).await;
        let reply = dispatch::parse(&next(&mut sdk, &mut sd).await.payload).unwrap();
        assert_eq!(reply.get("ok"), Some(&JsValue::Bool(true)));
        assert_eq!(
            reply.get("result"),
            Some(&object([("released", JsValue::Bool(released))]))
        );
        assert!(pending.is_pending());
    }
    send(&mut cli, release(JsValue::Null)).await;
    let reply = dispatch::parse(&next(&mut cli, &mut cd).await.payload).unwrap();
    assert_eq!(
        reply.get("error").unwrap().get("code"),
        Some(&"INVALID_USER_DIALOG_RELEASE".into())
    );
    assert!(pending.is_pending());
    channel.abort(pending.request_id());
    for (mut client, mut decoder, task) in [(cli, cd, ct), (sdk, sd, st)] {
        client
            .write_all(&frame::client_frame(0x88, 0, b""))
            .await
            .unwrap();
        assert_eq!(next(&mut client, &mut decoder).await.opcode, 8);
        drop(client);
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn user_dialog_respond_offline_cross_connection_wakes_channel_once() {
    use crate::user_dialog::{
        ChannelOptions, GatewayUserDialogChannel, GatewayUserDialogs, InputRequest,
    };
    let dialogs = Arc::new(GatewayUserDialogs::default());
    let mut options = ChannelOptions::new("s", dialogs.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "d".into());
    let channel = GatewayUserDialogChannel::new(options);
    let mut pending = channel.request_input(&InputRequest {
        tool_call_id: "c".into(),
        tool_name: "input".into(),
        prompt: "pending".into(),
        placeholder: None,
        allow_empty: false,
        abort: None,
    });
    let token = uuid::Uuid::new_v4().to_string();
    let server = Arc::new(
        {
            let mut server = LocalServer::with_token(
                Arc::new(Proxy {
                    upstream: "http://127.0.0.1:0".into(),
                    client: Client::builder(TokioExecutor::new()).build_http(),
                }),
                token.clone(),
            )
            .unwrap();
            server.interactions = Arc::default();
            server.dialogs = Arc::default();
            server
        }
        .with_user_dialogs(dialogs),
    );
    let (mut cli, mut cd, ct) = open(server.clone(), &token, "cli").await;
    next(&mut cli, &mut cd).await;
    let (mut sdk, mut sd, st) = open(server, &token, "sdk").await;
    next(&mut sdk, &mut sd).await;
    let params = object([
        ("sessionKey", "s".into()),
        ("requestId", "d".into()),
        ("ttlMs", JsValue::Number(1000.0)),
        ("interactionBinding", "caller-controlled".into()),
    ]);
    let request = |params| {
        object([
            ("type", "request".into()),
            ("id", "claim".into()),
            ("method", "user_dialog_claim".into()),
            ("params", params),
        ])
    };
    send(&mut cli, request(params.clone())).await;
    send(&mut sdk, request(params.clone())).await;
    let first = dispatch::parse(&next(&mut cli, &mut cd).await.payload).unwrap();
    let second = dispatch::parse(&next(&mut sdk, &mut sd).await.payload).unwrap();
    assert_eq!(first.get("ok"), Some(&JsValue::Bool(true)));
    assert_eq!(second.get("ok"), Some(&JsValue::Bool(true)));
    let a = first.get("result").unwrap();
    let b = second.get("result").unwrap();
    let (winner, loser) = if a.get("claimed") == Some(&JsValue::Bool(true)) {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(loser.get("reason"), Some(&"claimed".into()));
    let lease = winner.get("leaseId").unwrap().clone();
    let response = |lease, value| {
        object([
            ("type", "request".into()),
            ("id", "respond".into()),
            ("method", "user_dialog_respond".into()),
            (
                "params",
                object([
                    ("sessionKey", "s".into()),
                    ("requestId", "d".into()),
                    ("leaseId", lease),
                    (
                        "result",
                        object([("behavior", "answered".into()), ("value", value)]),
                    ),
                    ("interactionBinding", "raw-sdk".into()),
                ]),
            ),
        ])
    };
    for (bearer, value, code) in [
        (
            "wrong".into(),
            JsValue::Bool(false),
            "INVALID_USER_DIALOG_RESPONSE",
        ),
        ("wrong".into(), "yes".into(), "USER_DIALOG_LEASE_REQUIRED"),
    ] {
        send(&mut sdk, response(bearer, value)).await;
        let reply = dispatch::parse(&next(&mut sdk, &mut sd).await.payload).unwrap();
        assert_eq!(reply.get("error").unwrap().get("code"), Some(&code.into()));
        assert!(pending.is_pending());
    }
    send(&mut sdk, response(lease.clone(), "yes".into())).await;
    let reply = dispatch::parse(&next(&mut sdk, &mut sd).await.payload).unwrap();
    assert_eq!(
        reply.get("result"),
        Some(&object([("delivered", JsValue::Bool(true))]))
    );
    assert_eq!(
        pending.try_result().unwrap().unwrap(),
        crate::user_dialog::DialogAnswer::Answered("yes".into())
    );
    send(&mut cli, response(lease, "duplicate".into())).await;
    let reply = dispatch::parse(&next(&mut cli, &mut cd).await.payload).unwrap();
    assert_eq!(
        reply.get("result"),
        Some(&object([("delivered", JsValue::Bool(false))]))
    );
    for (mut client, mut decoder, task) in [(cli, cd, ct), (sdk, sd, st)] {
        client
            .write_all(&frame::client_frame(0x88, 0, b""))
            .await
            .unwrap();
        assert_eq!(next(&mut client, &mut decoder).await.opcode, 8);
        drop(client);
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
