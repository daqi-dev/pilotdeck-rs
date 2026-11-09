use super::*;
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicI64, Ordering},
    },
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("pdrs-perm-dialog-list-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn event(id: &str, prompt: &str) -> JsValue {
    object([
        ("type", "user_dialog_request".into()),
        ("requestId", id.into()),
        ("dialogKind", "input".into()),
        ("toolCallId", "c".into()),
        ("toolName", "input".into()),
        ("prompt", prompt.into()),
    ])
}
fn input() -> JsValue {
    object([("sessionKey", "s".into()), ("projectKey", "p".into())])
}
async fn listing(state: &GatewayUserDialogs, input: &JsValue) -> Vec<JsValue> {
    let reply = handle("user_dialog_list", input, state)
        .await
        .unwrap()
        .unwrap();
    let JsValue::Array(dialogs) = reply.get("dialogs").unwrap() else {
        panic!("dialogs array")
    };
    dialogs.clone()
}
fn ids(events: &[JsValue]) -> Vec<JsValue> {
    events
        .iter()
        .map(|event| {
            event
                .get("requestId")
                .or_else(|| {
                    event
                        .get("request")
                        .and_then(|request| request.get("requestId"))
                })
                .unwrap()
                .clone()
        })
        .collect()
}

#[tokio::test]
async fn user_dialog_list_native_and_http_share_channel_store_journal_and_recovery_order() {
    for http in [false, true] {
        let scratch = Scratch::new();
        let clock = Arc::new(AtomicI64::new(1_700_000_000_000));
        let runtime = DialogRuntime {
            now: {
                let clock = clock.clone();
                Arc::new(move || clock.load(Ordering::SeqCst))
            },
            ..Default::default()
        };
        let mut options = StoreOptions::new(scratch.0.join("store"));
        options.runtime = runtime.clone();
        let store = Arc::new(FileGatewayUserDialogStore::new(options).unwrap());
        let journal = Arc::new(GatewayUserDialogJournal::new(
            scratch.0.join("session.dialogs.json"),
            JournalOptions {
                runtime: runtime.clone(),
            },
        ));
        let key = StoreKey {
            project_root: scratch.0.join("project").to_str().unwrap().into(),
            pilot_home: scratch.0.join("home").to_str().unwrap().into(),
            session_id: "s".into(),
        };
        let state = GatewayUserDialogs {
            bus: Arc::new(Mutex::new(GatewayUserDialogBus::new(runtime.clone(), None))),
            persistence: None,
        };
        let mut channel_options = ChannelOptions::new("s", state.bus.clone(), Arc::new(|_| Ok(())));
        channel_options.uuid = Arc::new(|| "local".into());
        channel_options.now_ms = runtime.now.clone();
        channel_options.journal = Some(journal.clone());
        channel_options.store = Some(store.clone());
        channel_options.store_key = Some(key.clone());
        let channel = GatewayUserDialogChannel::new(channel_options);
        let pending = channel.request_input(&InputRequest {
            tool_call_id: "call".into(),
            tool_name: "input".into(),
            prompt: "from channel".into(),
            placeholder: None,
            allow_empty: false,
            abort: None,
        });
        assert!(pending.is_pending());
        // Same ID is local and hosted; the hosted projection must win.
        store
            .put(
                &key,
                &StoredDialog {
                    request: event("local", "host replaces local"),
                    created_at: "2000-01-01T00:00:00.000Z".into(),
                },
            )
            .unwrap();
        for (id, live) in [("hosted", true), ("stored", false)] {
            store
                .put(
                    &key,
                    &StoredDialog {
                        request: event(id, id),
                        created_at: "2001-01-01T00:00:00.000Z".into(),
                    },
                )
                .unwrap();
            if live {
                store
                    .claim_live_owner(&key, &id.into(), &"owner".into(), 5000.0)
                    .unwrap();
            }
        }
        store
            .claim_live_owner(&key, &"local".into(), &"owner".into(), 5000.0)
            .unwrap();
        journal.record(&event("journal", "journal first")).unwrap();
        journal
            .record(&event("stored", "journal loses to store"))
            .unwrap();
        let token = uuid::Uuid::new_v4().to_string();
        let server = if http {
            Some(
                start_gateway_user_dialog_store_http_server(store.clone(), &token, None, None)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let backend = match &server {
            Some(server) => DialogStore::Http(Arc::new(
                HttpGatewayUserDialogStore::new(HttpGatewayUserDialogStoreOptions {
                    url: server.url().into(),
                    authorization_token: token,
                    timeout_ms: Some(2000.0),
                })
                .unwrap(),
            )),
            None => DialogStore::Native(store.clone()),
        };
        let resolver: DialogSessionResolver = {
            let key = key.clone();
            let journal = journal.clone();
            Arc::new(move |params| {
                let session = params.get("sessionKey").unwrap().as_string().unwrap();
                if session.equals_str("ephemeral") {
                    return Ok(None);
                }
                let mut selected = key.clone();
                selected.session_id = session.clone();
                let same_project = params.get("projectKey") == Some(&JsValue::from("p"));
                if !same_project {
                    selected.project_root = "other-project".into();
                }
                Ok(Some(DialogSession {
                    key: selected,
                    journal: if same_project && session.equals_str("s") {
                        Some(journal.clone())
                    } else {
                        None
                    },
                }))
            })
        };
        let state = GatewayUserDialogs {
            persistence: Some(Arc::new(DialogPersistence::new(Some(backend), resolver))),
            ..state
        };
        // Lease disclosure is only expiresAt, never bearer leaseId.
        state
            .bus
            .lock()
            .unwrap()
            .claim(&"s".into(), &"local".into(), 1000.0, None)
            .unwrap();
        let result = listing(&state, &input()).await;
        assert_eq!(
            ids(&result),
            vec![
                "local".into(),
                "hosted".into(),
                "journal".into(),
                "stored".into()
            ]
        );
        assert_eq!(result[0].get("prompt"), Some(&"host replaces local".into()));
        assert!(result[0].get("lease").is_none());
        assert_eq!(
            result[3].get("terminatedAt"),
            Some(&"2001-01-01T00:00:00.000Z".into())
        );
        let first_time = result[2].get("terminatedAt").unwrap().clone();
        clock.fetch_add(5000, Ordering::SeqCst); // expiresAt == now makes hosted records recoverable.
        let after = listing(&state, &input()).await;
        assert_eq!(
            ids(&after),
            vec![
                "local".into(),
                "journal".into(),
                "stored".into(),
                "hosted".into()
            ]
        );
        assert!(after[0].get("lease").is_none()); // renderer lease also expired
        assert_eq!(after[1].get("terminatedAt"), Some(&first_time));
        assert_eq!(listing(&state, &input()).await, after); // cached timestamps are stable
        assert!(
            listing(
                &state,
                &object([("sessionKey", "other".into()), ("projectKey", "p".into())])
            )
            .await
            .is_empty()
        );
        assert!(
            listing(
                &state,
                &object([
                    ("sessionKey", "ephemeral".into()),
                    ("projectKey", "p".into())
                ])
            )
            .await
            .is_empty()
        );
        // Local bus is session-key scoped; hosted/recovery are additionally project scoped.
        assert_eq!(
            ids(&listing(
                &state,
                &object([("sessionKey", "s".into()), ("projectKey", "other".into())])
            )
            .await),
            vec!["local".into()]
        );
        channel.abort(pending.request_id());
        assert!(!pending.is_pending());
        assert!(!ids(&listing(&state, &input()).await).contains(&"local".into()));
        if let Some(server) = server {
            tokio::time::timeout(std::time::Duration::from_secs(3), server.close())
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn user_dialog_list_deduplicates_journal_ids_preserving_first_position_and_last_value() {
    let scratch = Scratch::new();
    let journal = Arc::new(GatewayUserDialogJournal::new(
        scratch.0.join("duplicate.dialogs.json"),
        JournalOptions::default(),
    ));
    let record = object([
        ("version", JsValue::Number(1.0)),
        (
            "dialogs",
            JsValue::Array(vec![
                event("same", "old"),
                event("second", "second"),
                event("same", "last"),
            ]),
        ),
    ]);
    std::fs::write(journal.path(), stringify_json(&record).unwrap().unwrap()).unwrap();
    let state = GatewayUserDialogs {
        persistence: Some(Arc::new(DialogPersistence::new(
            None,
            Arc::new(move |_| {
                Ok(Some(DialogSession {
                    key: StoreKey {
                        project_root: "project".into(),
                        pilot_home: "isolated".into(),
                        session_id: "s".into(),
                    },
                    journal: Some(journal.clone()),
                }))
            }),
        ))),
        ..Default::default()
    };
    let result = listing(&state, &input()).await;
    assert_eq!(ids(&result), vec!["same".into(), "second".into()]);
    assert_eq!(
        result[0].get("request").unwrap().get("prompt"),
        Some(&"last".into())
    );
}

#[tokio::test]
async fn user_dialog_list_validation_lease_projection_and_host_failures_are_local() {
    let state = GatewayUserDialogs::default();
    assert!(handle("unknown", &JsValue::Null, &state).await.is_none());
    for params in [JsValue::Undefined, JsValue::Null] {
        let error = handle("user_dialog_list", &params, &state)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "gateway_request_failed");
    }
    for key in [
        JsValue::Undefined,
        JsValue::Null,
        "".into(),
        " \u{feff}\u{2029}".into(),
    ] {
        let error = handle("user_dialog_list", &object([("sessionKey", key)]), &state)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "INVALID_USER_DIALOG_LIST");
        assert_eq!(error.message, "sessionKey is required.".into());
    }
    for key in [
        JsValue::Bool(false),
        JsValue::Number(7.0),
        JsValue::Array(vec![]),
        object([]),
    ] {
        let error = handle("user_dialog_list", &object([("sessionKey", key)]), &state)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "gateway_request_failed");
        assert_eq!(
            error.message,
            "input.sessionKey?.trim is not a function".into()
        );
    }
    let mut options = ChannelOptions::new(" s ", state.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "lease".into());
    let channel = GatewayUserDialogChannel::new(options);
    let pending = channel.request_input(&InputRequest {
        tool_call_id: "c".into(),
        tool_name: "input".into(),
        prompt: "p".into(),
        placeholder: None,
        allow_empty: true,
        abort: None,
    });
    state
        .bus
        .lock()
        .unwrap()
        .claim(&" s ".into(), pending.request_id(), 1000.0, None)
        .unwrap();
    let list = listing(&state, &object([("sessionKey", " s ".into())])).await;
    assert_eq!(ids(&list), vec!["lease".into()]);
    let JsValue::Object(lease) = list[0].get("lease").unwrap() else {
        panic!("lease")
    };
    assert_eq!(lease.entries().len(), 1);
    assert!(lease.get("leaseId").is_none());
    assert!(
        listing(&state, &object([("sessionKey", "s".into())]))
            .await
            .is_empty()
    );
    channel.abort(pending.request_id());
    let state = GatewayUserDialogs {
        persistence: Some(Arc::new(DialogPersistence::new(
            None,
            Arc::new(|_| Err(RequestError::new("host lookup failed"))),
        ))),
        ..state
    };
    let error = handle("user_dialog_list", &input(), &state)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.message, "host lookup failed".into());
}

fn claim_input(request: &str, ttl: JsValue, lease: JsValue) -> JsValue {
    object([
        ("sessionKey", "s".into()),
        ("requestId", request.into()),
        ("ttlMs", ttl),
        ("leaseId", lease),
    ])
}
async fn claiming(state: &GatewayUserDialogs, input: &JsValue) -> JsValue {
    handle("user_dialog_claim", input, state)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn user_dialog_claim_channel_file_http_compete_renew_and_expire_at_exact_boundary() {
    for mode in ["local", "file", "http"] {
        let scratch = Scratch::new();
        let clock = Arc::new(AtomicI64::new(1_700_000_000_000));
        let runtime = DialogRuntime {
            now: {
                let clock = clock.clone();
                Arc::new(move || clock.load(Ordering::SeqCst))
            },
            ..Default::default()
        };
        let mut options = StoreOptions::new(scratch.0.join("store"));
        options.runtime = runtime.clone();
        let store = Arc::new(FileGatewayUserDialogStore::new(options.clone()).unwrap());
        let key = StoreKey {
            project_root: "p".into(),
            pilot_home: "h".into(),
            session_id: "s".into(),
        };
        let bus = Arc::new(Mutex::new(GatewayUserDialogBus::new(runtime.clone(), None)));
        let mut channel_options = ChannelOptions::new("s", bus.clone(), Arc::new(|_| Ok(())));
        channel_options.uuid = Arc::new(|| "d".into());
        channel_options.now_ms = runtime.now.clone();
        if mode != "local" {
            channel_options.store = Some(store.clone());
            channel_options.store_key = Some(key.clone());
        }
        let channel = GatewayUserDialogChannel::new(channel_options);
        let pending = channel.request_input(&InputRequest {
            tool_call_id: "c".into(),
            tool_name: "input".into(),
            prompt: "claim me".into(),
            placeholder: None,
            allow_empty: false,
            abort: None,
        });
        assert!(pending.is_pending());
        if mode != "local" {
            store
                .claim_live_owner(&key, &"d".into(), &"owner".into(), 5000.0)
                .unwrap();
            assert!(
                !store
                    .renew_live_owner(&key, &"d".into(), &"wrong-owner".into(), 5000.0)
                    .unwrap()
            );
        }
        let token = uuid::Uuid::new_v4().to_string();
        let server = if mode == "http" {
            Some(
                start_gateway_user_dialog_store_http_server(store.clone(), &token, None, None)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let make_state = || GatewayUserDialogs {
            bus: bus.clone(),
            persistence: if mode == "local" {
                None
            } else {
                let backend = if let Some(server) = &server {
                    DialogStore::Http(Arc::new(
                        HttpGatewayUserDialogStore::new(HttpGatewayUserDialogStoreOptions {
                            url: server.url().into(),
                            authorization_token: token.clone(),
                            timeout_ms: Some(2000.0),
                        })
                        .unwrap(),
                    ))
                } else {
                    // Independent file handles must still serialize under the file lock.
                    DialogStore::Native(Arc::new(
                        FileGatewayUserDialogStore::new(options.clone()).unwrap(),
                    ))
                };
                let key = key.clone();
                Some(Arc::new(DialogPersistence::new(
                    Some(backend),
                    Arc::new(move |input| {
                        if input.get("projectKey") == Some(&"ephemeral".into()) {
                            return Ok(None);
                        }
                        let mut key = key.clone();
                        key.session_id = input
                            .get("sessionKey")
                            .unwrap()
                            .as_string()
                            .unwrap()
                            .clone();
                        if input.get("projectKey") == Some(&"other".into()) {
                            key.project_root = "other".into();
                        }
                        Ok(Some(DialogSession { key, journal: None }))
                    }),
                )))
            },
        };
        let first = make_state();
        let second = make_state();
        let input = claim_input("d", JsValue::Number(1000.0), JsValue::Undefined);
        let (a, b) = tokio::join!(claiming(&first, &input), claiming(&second, &input));
        let (winner, busy) = if a.get("claimed") == Some(&JsValue::Bool(true)) {
            (a, b)
        } else {
            (b, a)
        };
        assert_eq!(winner.get("claimed"), Some(&JsValue::Bool(true)), "{mode}");
        assert_eq!(
            busy,
            object([
                ("claimed", JsValue::Bool(false)),
                ("reason", "claimed".into()),
                (
                    "expiresAt",
                    JsValue::String(expiry(clock.load(Ordering::SeqCst), 1000.0).unwrap())
                )
            ])
        );
        let lease = winner.get("leaseId").unwrap().clone();
        assert_eq!(
            claiming(
                &first,
                &claim_input("d", JsValue::Number(1000.0), "wrong".into())
            )
            .await,
            busy
        );
        clock.fetch_add(500, Ordering::SeqCst);
        let padded = JsValue::String(
            format!(
                " \u{feff}{} ",
                lease.as_string().unwrap().to_utf8().unwrap()
            )
            .into(),
        );
        let padded_reply =
            claiming(&first, &claim_input("d", JsValue::Number(1000.0), padded)).await;
        if mode == "local" {
            assert_eq!(padded_reply.get("leaseId"), Some(&lease));
        } else {
            assert_eq!(padded_reply, busy);
        }
        let renewed = claiming(
            &second,
            &claim_input("d", JsValue::Number(300000.0), lease.clone()),
        )
        .await;
        assert_eq!(renewed.get("leaseId"), Some(&lease));
        assert_eq!(
            renewed.get("expiresAt"),
            Some(&JsValue::String(
                expiry(clock.load(Ordering::SeqCst), 300000.0).unwrap()
            ))
        );
        if mode != "local" {
            // Hosted wins over the same local pending ID without claiming the bus.
            assert!(
                bus.lock().unwrap().list(&"s".into())[0]
                    .get("lease")
                    .is_none()
            );
        }
        clock.fetch_add(299999, Ordering::SeqCst);
        assert_eq!(
            claiming(&first, &input).await.get("reason"),
            Some(&"claimed".into())
        );
        clock.fetch_add(1, Ordering::SeqCst);
        let fresh = claiming(&second, &input).await;
        assert_eq!(fresh.get("claimed"), Some(&JsValue::Bool(true)));
        assert_ne!(fresh.get("leaseId"), Some(&lease));
        assert_eq!(
            claiming(&first, &claim_input("d", JsValue::Number(1000.0), lease))
                .await
                .get("reason"),
            Some(&"claimed".into())
        );
        if mode != "local" {
            assert!(store.list_live(&key).unwrap().is_empty());
        } // owner expired; claimLive still owns the record
        let mut other = input.clone();
        set(&mut other, "sessionKey", " s ".into());
        assert_eq!(
            claiming(&first, &other).await.get("reason"),
            Some(&"not_pending".into())
        );
        let mut missing = input.clone();
        set(&mut missing, "requestId", " d ".into());
        assert_eq!(
            claiming(&first, &missing).await.get("reason"),
            Some(&"not_pending".into())
        );
        if mode != "local" {
            let mut isolated = input.clone();
            set(&mut isolated, "projectKey", "other".into());
            assert_eq!(
                claiming(&first, &isolated).await.get("claimed"),
                Some(&JsValue::Bool(true))
            ); // missing hosted falls back to local
            clock.fetch_add(1000, Ordering::SeqCst);
            set(&mut isolated, "projectKey", "ephemeral".into());
            assert_eq!(
                claiming(&first, &isolated).await.get("claimed"),
                Some(&JsValue::Bool(true))
            );
        }
        assert!(pending.is_pending());
        channel.abort(pending.request_id());
        assert_eq!(
            claiming(&first, &input).await,
            object([
                ("claimed", JsValue::Bool(false)),
                ("reason", "not_pending".into())
            ])
        );
        if let Some(server) = server {
            server.close().await;
        }
    }
}

#[tokio::test]
async fn user_dialog_claim_validation_order_defaults_and_host_errors() {
    let state = GatewayUserDialogs {
        persistence: Some(Arc::new(DialogPersistence::new(
            None,
            Arc::new(|_| panic!("invalid input reached host")),
        ))),
        ..Default::default()
    };
    for (input, code, message) in [
        (
            JsValue::Undefined,
            "gateway_request_failed",
            "Cannot read properties of undefined (reading 'sessionKey')",
        ),
        (
            JsValue::Null,
            "gateway_request_failed",
            "Cannot read properties of null (reading 'sessionKey')",
        ),
        (
            object([("requestId", JsValue::Number(7.0))]),
            "INVALID_USER_DIALOG_CLAIM",
            "sessionKey and requestId are required.",
        ),
        (
            object([("sessionKey", JsValue::Number(7.0))]),
            "gateway_request_failed",
            "input.sessionKey?.trim is not a function",
        ),
        (
            object([
                ("sessionKey", "s".into()),
                ("requestId", JsValue::Bool(false)),
            ]),
            "gateway_request_failed",
            "input.requestId?.trim is not a function",
        ),
        (
            claim_input(" \u{feff}", "bad".into(), JsValue::Number(7.0)),
            "INVALID_USER_DIALOG_CLAIM",
            "sessionKey and requestId are required.",
        ),
        (
            claim_input("d", "bad".into(), JsValue::Null),
            "INVALID_USER_DIALOG_CLAIM",
            "leaseId must be a non-empty string when provided.",
        ),
    ] {
        let error = handle("user_dialog_claim", &input, &state)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.message, JsString::from(message));
    }
    for lease in [
        "".into(),
        " \u{2029}".into(),
        JsValue::Bool(false),
        JsValue::Number(0.0),
        JsValue::Array(vec![]),
    ] {
        let error = handle(
            "user_dialog_claim",
            &claim_input("d", JsValue::Number(0.0), lease),
            &state,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.message,
            "leaseId must be a non-empty string when provided.".into()
        );
    }
    for ttl in [
        JsValue::Number(0.0),
        JsValue::Number(999.0),
        JsValue::Number(300001.0),
        JsValue::Number(1000.5),
        JsValue::Number(9007199254740992.0),
        JsValue::Number(f64::NAN),
        JsValue::Number(f64::INFINITY),
        "1000".into(),
        JsValue::Bool(true),
        JsValue::Array(vec![]),
    ] {
        let error = handle(
            "user_dialog_claim",
            &claim_input("d", ttl, JsValue::Undefined),
            &state,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.code, "INVALID_USER_DIALOG_CLAIM");
        assert_eq!(
            error.message,
            "ttlMs must be a safe integer between 1000 and 300000.".into()
        );
    }
    let state = GatewayUserDialogs {
        persistence: Some(Arc::new(DialogPersistence::new(
            None,
            Arc::new(|_| Err(RequestError::new("host lookup failed"))),
        ))),
        ..Default::default()
    };
    for ttl in [
        JsValue::Undefined,
        JsValue::Null,
        JsValue::Number(1000.0),
        JsValue::Number(300000.0),
    ] {
        let error = handle(
            "user_dialog_claim",
            &claim_input("d", ttl, JsValue::Undefined),
            &state,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.message, "host lookup failed".into());
    }
    let state = GatewayUserDialogs::default();
    let mut options = ChannelOptions::new("s", state.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "d".into());
    options.now_ms = Arc::new(|| 0);
    let channel = GatewayUserDialogChannel::new(options);
    let pending = channel.request_input(&InputRequest {
        tool_call_id: "c".into(),
        tool_name: "input".into(),
        prompt: "p".into(),
        placeholder: None,
        allow_empty: false,
        abort: None,
    });
    for ttl in [JsValue::Undefined, JsValue::Null] {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let reply = claiming(&state, &claim_input("d", ttl, JsValue::Undefined)).await;
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let at = parse_expiry(reply.get("expiresAt").unwrap().as_string().unwrap()).unwrap();
        assert!(at >= (before + 30000) as f64 && at <= (after + 30000) as f64);
        assert!(state.bus.lock().unwrap().release(
            &"s".into(),
            &"d".into(),
            reply.get("leaseId").unwrap().as_string().unwrap()
        ));
    }
    assert!(pending.is_pending());
    channel.abort(pending.request_id());
}

async fn releasing(state: &GatewayUserDialogs, lease: JsValue) -> bool {
    let result = handle(
        "user_dialog_release",
        &claim_input("d", JsValue::Undefined, lease),
        state,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.as_object().unwrap().entries().len(), 1);
    result.get("released").unwrap().as_bool().unwrap()
}

#[tokio::test]
async fn user_dialog_release_channel_file_http_bearers_expiry_and_local_priority() {
    for mode in ["local", "file", "http"] {
        let scratch = Scratch::new();
        let clock = Arc::new(AtomicI64::new(1_700_000_000_000));
        let runtime = DialogRuntime {
            now: {
                let clock = clock.clone();
                Arc::new(move || clock.load(Ordering::SeqCst))
            },
            ..Default::default()
        };
        let mut options = StoreOptions::new(scratch.0.join("store"));
        options.runtime = runtime.clone();
        let store = Arc::new(FileGatewayUserDialogStore::new(options).unwrap());
        let key = StoreKey {
            project_root: "p".into(),
            pilot_home: "h".into(),
            session_id: "s".into(),
        };
        let bus = Arc::new(Mutex::new(GatewayUserDialogBus::new(runtime.clone(), None)));
        let mut options = ChannelOptions::new("s", bus.clone(), Arc::new(|_| Ok(())));
        options.uuid = Arc::new(|| "d".into());
        options.now_ms = runtime.now.clone();
        if mode != "local" {
            options.store = Some(store.clone());
            options.store_key = Some(key.clone());
        }
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
        let server = if mode == "http" {
            Some(
                start_gateway_user_dialog_store_http_server(store.clone(), &token, None, None)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let host_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend = if let Some(server) = &server {
            DialogStore::Http(Arc::new(
                HttpGatewayUserDialogStore::new(HttpGatewayUserDialogStoreOptions {
                    url: server.url().into(),
                    authorization_token: token,
                    timeout_ms: Some(2000.0),
                })
                .unwrap(),
            ))
        } else {
            DialogStore::Native(store.clone())
        };
        let resolver = {
            let key = key.clone();
            let calls = host_calls.clone();
            Arc::new(move |params: &JsValue| {
                calls.fetch_add(1, Ordering::SeqCst);
                if params.get("projectKey") == Some(&"ephemeral".into()) {
                    return Ok(None);
                }
                let mut key = key.clone();
                key.session_id = params
                    .get("sessionKey")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .clone();
                if params.get("projectKey") == Some(&"other".into()) {
                    key.project_root = "other".into();
                }
                Ok(Some(DialogSession { key, journal: None }))
            })
        };
        let state = GatewayUserDialogs {
            bus: bus.clone(),
            persistence: if mode == "local" {
                None
            } else {
                Some(Arc::new(DialogPersistence::new(Some(backend), resolver)))
            },
        };
        let input = claim_input("d", JsValue::Number(1000.0), JsValue::Undefined);
        let claim = claiming(&state, &input).await;
        let lease = claim.get("leaseId").unwrap().clone();
        assert!(!releasing(&state, "wrong".into()).await);
        assert!(pending.is_pending());
        assert_eq!(
            claiming(&state, &input).await.get("reason"),
            Some(&"claimed".into())
        );
        let mut other = claim_input(" d ", JsValue::Undefined, lease.clone());
        assert_eq!(
            handle("user_dialog_release", &other, &state)
                .await
                .unwrap()
                .unwrap()
                .get("released"),
            Some(&JsValue::Bool(false))
        );
        set(&mut other, "requestId", "d".into());
        set(&mut other, "sessionKey", " s ".into());
        assert_eq!(
            handle("user_dialog_release", &other, &state)
                .await
                .unwrap()
                .unwrap()
                .get("released"),
            Some(&JsValue::Bool(false))
        );
        let padded = JsValue::String(
            format!(
                " \u{feff}{} ",
                lease.as_string().unwrap().to_utf8().unwrap()
            )
            .into(),
        );
        if mode == "local" {
            assert!(releasing(&state, padded).await);
        } else {
            assert!(!releasing(&state, padded).await); // hosted keeps the original bearer
            assert!(releasing(&state, lease.clone()).await);
        }
        assert!(!releasing(&state, lease).await);
        let claim = claiming(&state, &input).await;
        let lease = claim.get("leaseId").unwrap().clone();
        clock.fetch_add(999, Ordering::SeqCst);
        assert!(releasing(&state, lease).await);
        let claim = claiming(&state, &input).await;
        let expired = claim.get("leaseId").unwrap().clone();
        clock.fetch_add(1000, Ordering::SeqCst);
        assert!(!releasing(&state, expired.clone()).await); // expiresAt == now is expired
        let claim = claiming(&state, &input).await;
        let fresh = claim.get("leaseId").unwrap().clone();
        assert_ne!(fresh, expired);
        assert!(!releasing(&state, expired).await);
        if mode != "local" {
            // Two independent leases share one request; local success must leave hosted untouched.
            let local = bus
                .lock()
                .unwrap()
                .claim(&"s".into(), &"d".into(), 1000.0, None)
                .unwrap();
            let LeaseClaim::Claimed(local) = local else {
                panic!("local lease")
            };
            let before = host_calls.load(Ordering::SeqCst);
            assert!(releasing(&state, JsValue::String(local.lease_id)).await);
            assert_eq!(host_calls.load(Ordering::SeqCst), before);
            assert_eq!(
                claiming(&state, &input).await.get("reason"),
                Some(&"claimed".into())
            );
            for project in ["other", "ephemeral"] {
                let mut other = claim_input("d", JsValue::Undefined, fresh.clone());
                set(&mut other, "projectKey", project.into());
                assert_eq!(
                    handle("user_dialog_release", &other, &state)
                        .await
                        .unwrap()
                        .unwrap()
                        .get("released"),
                    Some(&JsValue::Bool(false))
                );
            }
        }
        let a = claim_input("d", JsValue::Undefined, fresh.clone());
        let (a, b) = tokio::join!(
            handle("user_dialog_release", &a, &state),
            handle("user_dialog_release", &a, &state)
        );
        let successes = [a, b]
            .into_iter()
            .filter(|reply| {
                reply.as_ref().unwrap().as_ref().unwrap().get("released")
                    == Some(&JsValue::Bool(true))
            })
            .count();
        assert_eq!(successes, 1);
        assert!(pending.is_pending());
        assert!(bus.lock().unwrap().has_pending(&"s".into(), &"d".into()));
        if mode != "local" {
            assert_eq!(store.list(&key).unwrap().len(), 1);
        }
        channel.abort(pending.request_id());
        assert!(!releasing(&state, fresh).await);
        if let Some(server) = server {
            server.close().await;
        }
    }
}

#[tokio::test]
async fn user_dialog_release_validation_order_and_local_short_circuit_host_errors() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = GatewayUserDialogs {
        persistence: Some(Arc::new(DialogPersistence::new(None, {
            let calls = calls.clone();
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(RequestError::new("host lookup failed"))
            })
        }))),
        ..Default::default()
    };
    for params in [JsValue::Undefined, JsValue::Null] {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        let error = handle("user_dialog_release", &params, &state)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "gateway_request_failed");
        assert_eq!(
            error.message,
            JsString::from(format!(
                "Cannot read properties of {kind} (reading 'sessionKey')"
            ))
        );
    }
    for name in ["sessionKey", "requestId", "leaseId"] {
        for value in [
            JsValue::Undefined,
            JsValue::Null,
            "".into(),
            " \u{feff}\u{2029}".into(),
            JsValue::Number(7.0),
            JsValue::Bool(false),
            JsValue::Array(vec![]),
            object([]),
        ] {
            let mut params = claim_input("d", JsValue::Undefined, "lease".into());
            set(&mut params, name, value.clone());
            let error = handle("user_dialog_release", &params, &state)
                .await
                .unwrap()
                .unwrap_err();
            if matches!(
                value,
                JsValue::Undefined | JsValue::Null | JsValue::String(_)
            ) {
                assert_eq!(error.code, "INVALID_USER_DIALOG_RELEASE");
                assert_eq!(
                    error.message,
                    "sessionKey, requestId, and leaseId are required.".into()
                );
            } else {
                assert_eq!(error.code, "gateway_request_failed");
                assert_eq!(
                    error.message,
                    JsString::from(format!("input.{name}?.trim is not a function"))
                );
            }
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut options = ChannelOptions::new("s", state.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "d".into());
    let channel = GatewayUserDialogChannel::new(options);
    let pending = channel.request_input(&InputRequest {
        tool_call_id: "c".into(),
        tool_name: "input".into(),
        prompt: "p".into(),
        placeholder: None,
        allow_empty: false,
        abort: None,
    });
    let LeaseClaim::Claimed(lease) = state
        .bus
        .lock()
        .unwrap()
        .claim(&"s".into(), &"d".into(), 300000.0, None)
        .unwrap()
    else {
        panic!("lease")
    };
    assert!(releasing(&state, JsValue::String(lease.lease_id)).await);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let error = handle(
        "user_dialog_release",
        &claim_input("d", JsValue::Undefined, "wrong".into()),
        &state,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.message, "host lookup failed".into());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(pending.is_pending());
    channel.abort(pending.request_id());
}
