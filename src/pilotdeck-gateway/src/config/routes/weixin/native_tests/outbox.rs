use super::*;

#[tokio::test]
async fn production_live_outbox_retries_failed_send_without_new_session_or_second_turn() {
    let server = server(true).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("/new next")]));
    let gateway = Arc::new(Gateway {
        turns: AtomicUsize::new(0),
        requests: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery.clone(),
    ));
    owner.process_batch(&credentials, batch.clone()).await.unwrap();
    assert_eq!(delivery.test_pending_len("chat"), 1);
    let first_session = storage.load_mapper().unwrap()["activeByChatId"]["chat"].clone();
    owner.process_batch(&credentials, batch).await.unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 1);
    assert_eq!(
        storage.load_mapper().unwrap()["activeByChatId"]["chat"],
        first_session
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn duplicate_permission_answer_cannot_decide_next_request_before_prompt_delivery() {
    struct Two {
        ready: tokio::sync::Notify,
        decisions: AtomicUsize,
        finished: tokio::sync::Notify,
    }
    impl ChannelGateway for Two {
        fn request<'a>(&'a self, method: &'a str, _params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async move {
                assert_eq!(method, "permission_decide");
                self.decisions.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"delivered":true}))
            })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!() })
        }
        fn stream_turn<'a>(
            &'a self,
            _params: Value,
            sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                for id in ["A", "B"] {
                    sink.event(json!({"type":"permission_request","requestId":id,"toolName":"write_file","payload":{}})).await?;
                }
                self.ready.notify_one();
                self.finished.notified().await;
                Ok(())
            })
        }
    }
    let server = server(false).await;
    server.hold_confirmation.store(true, Ordering::SeqCst);
    let (storage, credentials, batch) = setup(&server.base, json!([message("start")]));
    let gateway = Arc::new(Two {
        ready: Default::default(),
        decisions: Default::default(),
        finished: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let turn = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    gateway.ready.notified().await;
    let first = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let batch = second_batch(&storage, "1");
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    server.confirmation_entered.notified().await;
    // A completed turn must defer clearing the answer lock while the HTTP
    // confirmation is still in flight.
    gateway.finished.notify_one();
    turn.await.unwrap().unwrap();
    let mut duplicate = second_batch(&storage, "1");
    duplicate["id"] = "c".repeat(64).into();
    super::super::super::storage::write(
        &storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{}.json", "c".repeat(64))),
        &duplicate,
    )
    .unwrap();
    let second = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, duplicate).await })
    };
    tokio::task::yield_now().await;
    assert_eq!(gateway.decisions.load(Ordering::SeqCst), 1);
    server.hold_confirmation.store(false, Ordering::SeqCst);
    server.confirmation_release.notify_one();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(gateway.decisions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn live_owner_ignores_non_user_system_and_echo_messages() {
    let server = server(false).await;
    let mut echo = message("must not execute");
    echo["message_type"] = 2.into();
    let (storage, credentials, batch) = setup(&server.base, json!([echo]));
    let gateway = Arc::new(Gateway {
        turns: Default::default(),
        requests: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage,
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    owner.process_batch(&credentials, batch).await.unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 0);
    assert!(gateway.requests.lock().unwrap().is_empty());
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn new_session_drops_ordinary_message_queued_under_previous_generation() {
    struct Hold {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        turns: AtomicUsize,
    }
    impl ChannelGateway for Hold {
        fn request<'a>(&'a self, method: &'a str, _params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async move {
                assert_eq!(method, "abort_turn");
                self.release.notify_one();
                Ok(json!({}))
            })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!() })
        }
        fn stream_turn<'a>(
            &'a self,
            _params: Value,
            _sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                self.turns.fetch_add(1, Ordering::SeqCst);
                self.entered.notify_one();
                self.release.notified().await;
                Ok(())
            })
        }
    }
    let server = server(false).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("active")]));
    let gateway = Arc::new(Hold {
        entered: Default::default(),
        release: Default::default(),
        turns: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let active = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    gateway.entered.notified().await;
    let queued = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let batch = second_batch(&storage, "old queued");
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    let mut reset = second_batch(&storage, "/new");
    reset["id"] = "d".repeat(64).into();
    super::super::super::storage::write(
        &storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{}.json", "d".repeat(64))),
        &reset,
    )
    .unwrap();
    owner.process_batch(&credentials, reset).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    active.await.unwrap().unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn new_session_clears_initial_prompt_gate_before_accepting_ordinary_message() {
    struct Prompt {
        turns: AtomicUsize,
    }
    impl ChannelGateway for Prompt {
        fn request<'a>(&'a self, method: &'a str, _params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async move {
                assert_eq!(method, "abort_turn");
                Ok(json!({}))
            })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!() })
        }
        fn stream_turn<'a>(
            &'a self,
            _params: Value,
            sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
                    sink.event(json!({"type":"permission_request","requestId":"old","toolName":"write_file","payload":{}})).await?;
                }
                Ok(())
            })
        }
    }
    let server = server(false).await;
    server.hold_prompt.store(true, Ordering::SeqCst);
    let (storage, credentials, batch) = setup(&server.base, json!([message("active")]));
    let gateway = Arc::new(Prompt {
        turns: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let active = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    server.prompt_entered.notified().await;
    let reset = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let batch = second_batch(&storage, "/new");
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    tokio::task::yield_now().await;
    server.hold_prompt.store(false, Ordering::SeqCst);
    server.prompt_release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), reset)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    active.await.unwrap().unwrap();
    let mut ordinary = second_batch(&storage, "after new");
    ordinary["id"] = "e".repeat(64).into();
    super::super::super::storage::write(
        &storage
            .home
            .join("channels/weixin-inbox")
            .join(format!("{}.json", "e".repeat(64))),
        &ordinary,
    )
    .unwrap();
    owner.process_batch(&credentials, ordinary).await.unwrap();
    assert_eq!(gateway.turns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn new_session_preempts_blocked_permission_rpc_and_ignores_its_stale_reply() {
    struct Blocked {
        turns: AtomicUsize,
        ready: tokio::sync::Notify,
        deciding: tokio::sync::Notify,
        decision_release: tokio::sync::Notify,
        old_turn_release: tokio::sync::Notify,
        new_turn_release: tokio::sync::Notify,
        decisions: StdMutex<Vec<Value>>,
    }
    impl ChannelGateway for Blocked {
        fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async move {
                match method {
                    "abort_turn" => self.old_turn_release.notify_one(),
                    "permission_decide" => {
                        self.decisions.lock().unwrap().push(params.clone());
                        if params["requestId"] == "old" {
                            self.deciding.notify_one();
                            self.decision_release.notified().await;
                        }
                    }
                    _ => panic!("unexpected RPC {method}"),
                }
                Ok(json!({"delivered":true}))
            })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!() })
        }
        fn stream_turn<'a>(
            &'a self,
            _params: Value,
            sink: &'a dyn crate::config::channel_commands::ChannelEventSink,
        ) -> ChannelCall<'a, ()> {
            Box::pin(async move {
                let old = self.turns.fetch_add(1, Ordering::SeqCst) == 0;
                sink.event(json!({"type":"permission_request","requestId":if old {"old"}else{"new"},"toolName":"write_file","payload":{}})).await?;
                self.ready.notify_one();
                if old {
                    self.old_turn_release.notified().await;
                } else {
                    self.new_turn_release.notified().await;
                }
                Ok(())
            })
        }
    }
    fn batch(storage: &Storage, id: char, input: &str) -> Value {
        let mut batch = second_batch(storage, input);
        batch["id"] = id.to_string().repeat(64).into();
        super::super::super::storage::write(
            &storage
                .home
                .join("channels/weixin-inbox")
                .join(format!("{}.json", id.to_string().repeat(64))),
            &batch,
        )
        .unwrap();
        batch
    }
    let server = server(false).await;
    let (storage, credentials, first) = setup(&server.base, json!([message("start")]));
    let gateway = Arc::new(Blocked {
        turns: Default::default(),
        ready: Default::default(),
        deciding: Default::default(),
        decision_release: Default::default(),
        old_turn_release: Default::default(),
        new_turn_release: Default::default(),
        decisions: Default::default(),
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery,
    ));
    let old_turn = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, first).await })
    };
    gateway.ready.notified().await;
    let old_answer = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let answer = batch(&storage, 'b', "1");
        tokio::spawn(async move { owner.process_batch(&credentials, answer).await })
    };
    gateway.deciding.notified().await;
    tokio::time::timeout(
        Duration::from_secs(3),
        owner.process_batch(&credentials, batch(&storage, 'c', "/new")),
    )
    .await
    .unwrap()
    .unwrap();
    old_turn.await.unwrap().unwrap();
    let new_turn = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        let input = batch(&storage, 'd', "after new");
        tokio::spawn(async move { owner.process_batch(&credentials, input).await })
    };
    gateway.ready.notified().await;
    gateway.decision_release.notify_one();
    old_answer.await.unwrap().unwrap();
    assert!(
        !server.requests.lock().unwrap().iter().any(
            |request| request["msg"]["item_list"][0]["text_item"]["text"]
                == "已允许一次，继续执行。"
        )
    );
    owner
        .process_batch(&credentials, batch(&storage, 'e', "1"))
        .await
        .unwrap();
    assert_eq!(
        gateway
            .decisions
            .lock()
            .unwrap()
            .iter()
            .map(|value| value["requestId"].clone())
            .collect::<Vec<_>>(),
        vec![json!("old"), json!("new")]
    );
    gateway.new_turn_release.notify_one();
    new_turn.await.unwrap().unwrap();
}

#[tokio::test]
async fn production_weixin_cron_empty_origin_session_does_not_use_session_key() {
    let server = server(false).await;
    let (storage, _, _) = setup(&server.base, json!([]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    delivery.test_seed_context("chat", "ctx-token");
    assert!(
        !delivery
            .deliver(&json!({
                "originChannelKey":"weixin",
                "originSessionKey":"",
                "sessionKey":"weixin:chat=chat:general",
                "text":"must not send"
            }))
            .await
            .unwrap()
    );
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn live_reply_without_context_queues_pending() {
    let server = server(false).await;
    let (storage, credentials, _) = setup(&server.base, json!([]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    super::send_reply_with_queue(
        &storage,
        &Http::new(),
        &delivery,
        &credentials,
        "chat",
        None,
        "queued live reply",
        true,
    )
    .await
    .unwrap();
    assert_eq!(delivery.test_pending_len("chat"), 1);
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn attachment_fallback_without_context_is_queued_not_err() {
    let server = server(false).await;
    let (storage, _, _) = setup(&server.base, json!([]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    delivery.queue_attachment_fallback(
        "chat",
        &json!({"name":"doc.pdf","path":"/tmp/doc.pdf"}),
    );
    assert_eq!(delivery.test_pending_len("chat"), 1);
    assert!(server.requests.lock().unwrap().is_empty());
}

/// TS `WeixinChannel.ts:731` + `ImPermissionHelper.ts:99-102` — permission send without queue.
#[tokio::test]
async fn permission_prompt_send_failure_sets_prompt_retry_not_queue() {
    let server = server(true).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("start")]));
    let gateway = Arc::new(InteractiveGateway {
        requested: Default::default(),
        answered: Default::default(),
        requests: Default::default(),
        elicitation: false,
    });
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        gateway.clone(),
        ChannelSessionState::default(),
        delivery.clone(),
    ));
    let turn = {
        let owner = owner.clone();
        let credentials = credentials.clone();
        tokio::spawn(async move { owner.process_batch(&credentials, batch).await })
    };
    gateway.requested.notified().await;
    assert_eq!(delivery.test_pending_len("chat"), 0);
    owner
        .process_batch(&credentials, second_batch(&storage, "1"))
        .await
        .unwrap();
    assert!(gateway.requests.lock().unwrap().is_empty());
    turn.abort();
}

#[tokio::test]
async fn production_weixin_cron_send_failure_returns_false_and_queues() {
    let server = server(true).await;
    let (storage, credentials, _) = setup(&server.base, json!([]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    delivery.test_seed_context("chat", "ctx-token");
    assert!(
        !delivery
            .deliver(&json!({
                "originChannelKey":"weixin",
                "originSessionKey":"weixin:chat=chat:general",
                "text":"cron failed send"
            }))
            .await
            .unwrap()
    );
    assert_eq!(delivery.test_pending_len("chat"), 1);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    delivery.flush_pending_replies(&credentials).await;
    assert_eq!(delivery.test_pending_len("chat"), 0);
    assert!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|v| v["msg"]["item_list"][0]["text_item"]["text"] == "cron failed send")
    );
}

/// `WeixinChannel.ts:431` — pure `/new` confirmation must not enter pending queue.
#[tokio::test]
async fn pure_new_confirmation_failure_does_not_queue() {
    let server = server(true).await;
    let (storage, credentials, batch) = setup(&server.base, json!([message("/new")]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        Arc::new(Gateway {
            turns: AtomicUsize::new(0),
            requests: Default::default(),
        }),
        ChannelSessionState::default(),
        delivery.clone(),
    ));
    owner.process_batch(&credentials, batch).await.unwrap();
    assert_eq!(delivery.test_pending_len("chat"), 0);
}

/// `WeixinChannel.ts:789` — each outbox chunk queues independently on failure.
#[tokio::test]
async fn deliver_outbox_queues_every_failed_chunk() {
    let server = server_reject_all_sends().await;
    let (storage, credentials, _) = setup(&server.base, json!([]));
    let delivery = NativeWeixinFactory::new(storage.home.clone(), Some(server.base.clone()))
        .unwrap()
        .delivery();
    let owner = Arc::new(live::LiveOwner::new(
        storage.clone(),
        Http::new(),
        Arc::new(Gateway {
            turns: AtomicUsize::new(0),
            requests: Default::default(),
        }),
        ChannelSessionState::default(),
        delivery.clone(),
    ));
    delivery.test_seed_context("chat", "context-fixture");
    let receipt_path = storage
        .home
        .join("channels/weixin-inbox")
        .join("outbox-chunks-0.receipt");
    let mut receipt = json!({
        "outbox": ["chunk-a", "chunk-b"],
        "sentIndex": 0
    });
    super::super::super::storage::write(&receipt_path, &receipt).unwrap();
    owner
        .deliver_outbox(
            &credentials,
            "chat",
            &receipt_path,
            &mut receipt,
        )
        .await
        .unwrap();
    assert_eq!(delivery.test_pending_len("chat"), 2);
    assert!(receipt["complete"].as_bool().unwrap_or(false) == false);
}

#[tokio::test]
async fn production_weixin_cron_invalid_credentials_returns_ok_false_not_err() {
    let server = server(false).await;
    let home = std::env::temp_dir().join(format!("pdrs-wx-bad-cred-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(home.join("channels")).unwrap();
    std::fs::write(home.join("weixin-credentials.json"), b"not-json").unwrap();
    let delivery = NativeWeixinFactory::new(home, Some(server.base.clone()))
        .unwrap()
        .delivery();
    delivery.test_seed_context("chat", "ctx-token");
    let result = delivery
        .deliver(&json!({
            "originChannelKey":"weixin",
            "originSessionKey":"weixin:chat=chat:general",
            "text":"queued on bad creds"
        }))
        .await;
    assert!(result.is_ok());
    assert!(!result.unwrap());
    assert_eq!(delivery.test_pending_len("chat"), 1);
    assert!(server.requests.lock().unwrap().is_empty());
}
