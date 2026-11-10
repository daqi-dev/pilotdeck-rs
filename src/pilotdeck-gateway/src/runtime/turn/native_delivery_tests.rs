use super::*;

async fn timeout_entry(native: bool) {
    let root = std::env::temp_dir().join(format!("pdrs-native-delivery-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("registry.json");
    let registry = Arc::new(Registry {
        file: FileRegistry::new(FileRunRegistry::open(&path).unwrap()).unwrap(),
        fail: false,
        pending: Arc::default(),
        path,
    });
    let port = registry.clone();
    let (send, ready) = std::sync::mpsc::sync_channel(1);
    let build = move |coordinator: Arc<TurnEventCoordinator>| {
        let session = Arc::new(Session {
            thread: std::thread::current().id(),
            native: vec![
                js(&json!({"type":"turn_started","sessionId":"s","turnId":"r"})),
                js(&json!({"type":"input_accepted","sessionId":"s","turnId":"r","messages":[]})),
            ],
            coordinator: coordinator.clone(),
            accepted: Gate::new(),
            terminal: Gate::new(),
            end: Gate::new(),
            aborts: AtomicUsize::new(0),
        });
        let mut options = SessionOwnerOptions::new(Arc::new(Factory(session.clone())));
        options.idle_sweep_interval_ms = 0;
        let owner = SessionOwner::new(options);
        let interactions = Arc::new(GatewayInteractionCoordinator::default());
        let submitter = Arc::new(TurnSubmitter {
            owner,
            coordinator,
            registry: Some(port),
            projector: Arc::new(GatewayAgentEventProjector::new(Default::default()).unwrap()),
            host: Arc::new(Host {
                interactions: interactions.clone(),
                finally_failure: false,
            }),
            workspace_admissions: WorkspaceAdmissions::default(),
        });
        send.send(session).unwrap();
        Ok(GatewayServices {
            submitter,
            interactions,
            requests: Arc::new(Requests),
        })
    };
    let host = if native {
        TurnGateway::start_native_with_initializer(
            TurnReplayOptions::new(),
            Duration::from_secs(3),
            build,
            async { Ok(()) },
            async { Ok(()) },
        )
    } else {
        TurnGateway::start_with_initializer(
            TurnReplayOptions::new(),
            Duration::from_secs(3),
            build,
            async { Ok(()) },
            async { Ok(()) },
        )
    }
    .unwrap();
    let session = ready.recv_timeout(Duration::from_secs(3)).unwrap();
    session.accepted.open();
    let text = Arc::new(Text::default());
    let connection = host.connect(
        text.clone(),
        "token".into(),
        "native-delivery-fixture".into(),
    );
    connection.receive(json!({"type":"hello","protocolVersion":GATEWAY_PROTOCOL_VERSION,"token":"token","clientName":"web","clientVersion":"fixture"}).to_string()).await.unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    let progress = text.clone();
    let consumer = tokio::spawn(async move {
        connection.receive(request("turn", &json!({"sessionKey":"s","projectKey":"p","runId":"r","message":"hello","channelKey":"test","timeoutMs":200}))).await.unwrap();
        finished.store(true, Ordering::SeqCst);
        progress.changed();
    });
    tokio::time::timeout(
        Duration::from_secs(3),
        text.observation_ready(2, false, &done),
    )
    .await
    .unwrap();
    {
        let drains = host.gateway.1.lock().unwrap();
        let id = *drains
            .current
            .get(&JsString::from("s"))
            .expect("admission observer binds this session");
        assert_eq!(
            drains.active[&id].0.as_ref().unwrap().run_id,
            JsString::from("r")
        );
    }
    tokio::time::timeout(Duration::from_secs(3), consumer)
        .await
        .unwrap()
        .unwrap();
    let labels = text
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|raw| serde_json::from_str::<Value>(raw).unwrap())
        .filter(|f| f["type"] == "event" && f["final"] == false)
        .map(|f| f["event"].clone())
        .filter(|e| {
            !matches!(
                e["type"].as_str(),
                Some(
                    "turn_started"
                        | "input_accepted"
                        | "model_request_started"
                        | "tool_call_started"
                )
            )
        })
        .map(|e| {
            format!(
                "{}:{}",
                e["type"].as_str().unwrap(),
                e["event"]
                    .as_str()
                    .or_else(|| e["code"].as_str())
                    .or_else(|| e["finishReason"].as_str())
                    .unwrap_or("")
            )
        })
        .collect::<Vec<_>>();
    if native {
        assert_eq!(
            labels,
            ["agent_status:turn_timeout", "error:turn_timeout"],
            "native entry retains exact timeout order after real registry persistence"
        );
    } else {
        assert!(
            labels.is_empty(),
            "legacy TS parity entry still drops async timeout notifications at close: {labels:?}"
        );
    }
    assert!(
        host.gateway.1.lock().unwrap().active.is_empty(),
        "consumer finally releases its real drain guard"
    );
    assert!(
        host.gateway.1.lock().unwrap().current.is_empty(),
        "admission registration is not leaked"
    );
    tokio::time::timeout(
        Duration::from_secs(3),
        host.shutdown(Duration::from_secs(2)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(registry.pending.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn native_initializer_retains_exact_timeout_notifications_and_real_drain_observer() {
    timeout_entry(true).await;
}

#[tokio::test]
async fn legacy_initializer_preserves_ts_timeout_queue_close() {
    timeout_entry(false).await;
}
