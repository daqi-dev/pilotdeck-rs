use super::*;
use crate::runtime::connection::RequestError;
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicI64, AtomicUsize, Ordering},
    },
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pdrs-respond-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn params(id: &str, result: JsValue, lease: JsValue) -> JsValue {
    object([
        ("sessionKey", "s".into()),
        ("requestId", id.into()),
        ("result", result),
        ("leaseId", lease),
        ("interactionBinding", "sdk-original".into()),
    ])
}
fn answered(value: JsValue) -> JsValue {
    object([("behavior", "answered".into()), ("value", value)])
}
async fn respond(
    state: &GatewayUserDialogs,
    params: &JsValue,
) -> std::result::Result<JsValue, RequestError> {
    routes::handle("user_dialog_respond", params, state)
        .await
        .unwrap()
}
fn delivered(value: bool) -> JsValue {
    object([("delivered", JsValue::Bool(value))])
}
fn input(channel: &GatewayUserDialogChannel) -> ChannelRequest {
    channel.request_input(&InputRequest {
        tool_call_id: "c".into(),
        tool_name: "input".into(),
        prompt: "p".into(),
        placeholder: None,
        allow_empty: false,
        abort: None,
    })
}
#[tokio::test]
async fn user_dialog_respond_validation_order_and_falsy_values() {
    let state = GatewayUserDialogs {
        persistence: Some(Arc::new(DialogPersistence::new(
            None,
            Arc::new(|_| panic!("invalid reached host")),
        ))),
        ..Default::default()
    };
    let cases = [
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
            object([]),
            "INVALID_USER_DIALOG_RESPONSE",
            "sessionKey and requestId are required.",
        ),
        (
            object([("sessionKey", JsValue::Number(7.0))]),
            "gateway_request_failed",
            "input.sessionKey?.trim is not a function",
        ),
        (
            params(" ", JsValue::Null, JsValue::Null),
            "INVALID_USER_DIALOG_RESPONSE",
            "sessionKey and requestId are required.",
        ),
        (
            params("d", JsValue::Null, JsValue::Null),
            "INVALID_USER_DIALOG_RESPONSE",
            "result.behavior must be answered or cancelled.",
        ),
        (
            params("d", answered(JsValue::Undefined), JsValue::Null),
            "INVALID_USER_DIALOG_RESPONSE",
            "answered user dialog results require a value.",
        ),
        (
            params(
                "d",
                object([("behavior", "cancelled".into()), ("reason", JsValue::Null)]),
                JsValue::Null,
            ),
            "INVALID_USER_DIALOG_RESPONSE",
            "cancelled user dialog reason must be a string.",
        ),
        (
            params("d", answered(JsValue::Null), JsValue::Null),
            "INVALID_USER_DIALOG_RESPONSE",
            "leaseId must be a non-empty string when provided.",
        ),
    ];
    for (params, code, message) in cases {
        let error = respond(&state, &params).await.unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.message, JsString::from(message));
    }
    let state = GatewayUserDialogs::default();
    for value in [
        JsValue::Null,
        JsValue::Bool(false),
        JsValue::Number(0.0),
        "".into(),
    ] {
        assert_eq!(
            respond(&state, &params("none", answered(value), JsValue::Undefined))
                .await
                .unwrap(),
            delivered(false)
        );
    }
}

#[tokio::test]
async fn user_dialog_respond_local_validation_lease_once_cancel_and_wakeup() {
    let clock = Arc::new(AtomicI64::new(1000));
    let bus = Arc::new(Mutex::new(GatewayUserDialogBus::new(
        DialogRuntime {
            now: {
                let c = clock.clone();
                Arc::new(move || c.load(Ordering::SeqCst))
            },
            ..Default::default()
        },
        None,
    )));
    let state = GatewayUserDialogs {
        bus: bus.clone(),
        persistence: None,
    };
    let mut options = ChannelOptions::new("s", bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "d".into());
    let channel = GatewayUserDialogChannel::new(options);
    let mut pending = input(&channel);
    let LeaseClaim::Claimed(lease) = bus
        .lock()
        .unwrap()
        .claim(&"s".into(), &"d".into(), 1000.0, None)
        .unwrap()
    else {
        panic!()
    };
    let error = respond(
        &state,
        &params("d", answered(JsValue::Bool(false)), "wrong".into()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "INVALID_USER_DIALOG_RESPONSE");
    for lease in [JsValue::Undefined, "wrong".into()] {
        assert_eq!(
            respond(&state, &params("d", answered("yes".into()), lease))
                .await
                .unwrap_err()
                .code,
            "USER_DIALOG_LEASE_REQUIRED"
        );
        assert!(pending.is_pending());
    }
    let padded = JsValue::String(format!(" {} ", lease.lease_id.to_utf8().unwrap()).into());
    let p = params("d", answered("yes".into()), padded);
    let (a, b) = tokio::join!(respond(&state, &p), respond(&state, &p));
    assert_eq!(
        [a.unwrap(), b.unwrap()]
            .iter()
            .filter(|x| **x == delivered(true))
            .count(),
        1
    );
    assert_eq!(
        pending.try_result().unwrap().unwrap(),
        DialogAnswer::Answered("yes".into())
    );
    assert!(pending.try_result().is_none());
    let mut pending = input(&channel);
    bus.lock()
        .unwrap()
        .claim(&"s".into(), &"d".into(), 1000.0, None)
        .unwrap();
    clock.store(2000, Ordering::SeqCst);
    assert_eq!(
        respond(
            &state,
            &params(
                "d",
                object([("behavior", "cancelled".into()), ("reason", "".into())]),
                JsValue::Undefined
            )
        )
        .await
        .unwrap(),
        delivered(true)
    );
    assert_eq!(
        pending.try_result().unwrap().unwrap(),
        DialogAnswer::Cancelled(None)
    );
    let mut pending = input(&channel);
    assert_eq!(
        respond(
            &state,
            &params(
                "d",
                object([("behavior", "cancelled".into()), ("reason", "stop".into())]),
                JsValue::Undefined
            )
        )
        .await
        .unwrap(),
        delivered(true)
    );
    assert_eq!(
        pending.try_result().unwrap().unwrap(),
        DialogAnswer::Cancelled(Some("stop".into()))
    );
}

#[tokio::test]
async fn user_dialog_respond_channel_cleanup_io_failure_keeps_pending_for_retry() {
    let scratch = Scratch::new();
    let path = scratch.0.join("dialogs.json");
    let journal = Arc::new(GatewayUserDialogJournal::new(
        path.clone(),
        JournalOptions::default(),
    ));
    let state = GatewayUserDialogs::default();
    let mut options = ChannelOptions::new("s", state.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "d".into());
    options.journal = Some(journal.clone());
    let channel = GatewayUserDialogChannel::new(options);
    let mut pending = input(&channel);
    let saved = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        respond(
            &state,
            &params("d", answered("yes".into()), JsValue::Undefined)
        )
        .await
        .is_err()
    );
    assert!(pending.is_pending());
    assert!(
        state
            .bus
            .lock()
            .unwrap()
            .has_pending(&"s".into(), &"d".into())
    );
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, saved).unwrap();
    assert_eq!(
        respond(
            &state,
            &params("d", answered("yes".into()), JsValue::Undefined)
        )
        .await
        .unwrap(),
        delivered(true)
    );
    assert!(pending.try_result().unwrap().is_ok());
    assert!(journal.recover().unwrap().is_empty());
}

#[tokio::test]
async fn user_dialog_respond_form_default_enforces_contract_before_consuming() {
    let state = GatewayUserDialogs::default();
    let mut options = ChannelOptions::new("s", state.bus.clone(), Arc::new(|_| Ok(())));
    options.uuid = Arc::new(|| "f".into());
    let channel = GatewayUserDialogChannel::new(options);
    let schema=pilotdeck_protocol::parse_json(r#"{"type":"object","required":["n"],"properties":{"n":{"type":"integer","minimum":1}},"additionalProperties":false}"#).unwrap();
    let mut pending = channel.request_form(&FormRequest {
        tool_call_id: "c".into(),
        tool_name: "form".into(),
        prompt: "p".into(),
        schema,
        abort: None,
    });
    for value in [
        JsValue::Null,
        JsValue::Array(vec![]),
        object([]),
        object([("n", JsValue::Number(0.0))]),
    ] {
        assert_eq!(
            respond(&state, &params("f", answered(value), JsValue::Undefined))
                .await
                .unwrap_err()
                .code,
            "INVALID_USER_DIALOG_RESPONSE"
        );
        assert!(pending.is_pending());
    }
    let value = object([("n", JsValue::Number(1.0))]);
    assert_eq!(
        respond(
            &state,
            &params("f", answered(value.clone()), JsValue::Undefined)
        )
        .await
        .unwrap(),
        delivered(true)
    );
    assert_eq!(
        pending.try_result().unwrap().unwrap(),
        DialogAnswer::Answered(value)
    );
}

#[tokio::test]
async fn user_dialog_respond_file_http_hosted_priority_bearer_handoff_and_repeat() {
    for http in [false, true] {
        let scratch = Scratch::new();
        let clock = Arc::new(AtomicI64::new(1700000000000));
        let runtime = DialogRuntime {
            now: {
                let c = clock.clone();
                Arc::new(move || c.load(Ordering::SeqCst))
            },
            ..Default::default()
        };
        let mut opts = StoreOptions::new(scratch.0.join("store"));
        opts.runtime = runtime.clone();
        let store = Arc::new(FileGatewayUserDialogStore::new(opts).unwrap());
        let key = StoreKey {
            project_root: "p".into(),
            pilot_home: "h".into(),
            session_id: "s".into(),
        };
        let bus = Arc::new(Mutex::new(GatewayUserDialogBus::new(runtime.clone(), None)));
        let mut opts = ChannelOptions::new("s", bus.clone(), Arc::new(|_| Ok(())));
        opts.uuid = Arc::new(|| "d".into());
        opts.now_ms = runtime.now.clone();
        opts.store = Some(store.clone());
        opts.store_key = Some(key.clone());
        opts.take_stored_answer = Some({
            let store = store.clone();
            let key = key.clone();
            Arc::new(move |id| store.take_live_answer(&key, id))
        });
        let channel = GatewayUserDialogChannel::new(opts);
        let mut pending = input(&channel);
        store
            .claim_live_owner(&key, &"d".into(), &"owner".into(), 5000.0)
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
        let host = DialogPersistence::new(Some(backend), {
            let key = key.clone();
            Arc::new(move |input| {
                assert_eq!(
                    input.get("interactionBinding"),
                    Some(&"sdk-original".into())
                );
                Ok(Some(DialogSession {
                    key: key.clone(),
                    journal: None,
                }))
            })
        });
        let state = GatewayUserDialogs {
            bus: bus.clone(),
            persistence: Some(Arc::new(host)),
        };
        let LeaseClaim::Claimed(lease) = store.claim_live(&key, &"d".into(), 1000.0, None).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            respond(
                &state,
                &params(
                    "d",
                    answered(JsValue::Bool(false)),
                    JsValue::String(lease.lease_id.clone())
                )
            )
            .await
            .unwrap_err()
            .code,
            "INVALID_USER_DIALOG_RESPONSE"
        );
        assert!(pending.is_pending());
        let LeaseClaim::Claimed(local) = bus
            .lock()
            .unwrap()
            .claim(&"s".into(), &"d".into(), 1000.0, None)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            respond(
                &state,
                &params("d", answered("answer".into()), "wrong".into())
            )
            .await
            .unwrap_err()
            .code,
            "USER_DIALOG_LEASE_REQUIRED"
        );
        let padded = JsValue::String(format!(" {} ", lease.lease_id.to_utf8().unwrap()).into());
        assert_eq!(
            respond(&state, &params("d", answered("answer".into()), padded))
                .await
                .unwrap_err()
                .code,
            "USER_DIALOG_LEASE_REQUIRED"
        );
        for value in ["first", "last"] {
            assert_eq!(
                respond(
                    &state,
                    &params(
                        "d",
                        answered(value.into()),
                        JsValue::String(lease.lease_id.clone())
                    )
                )
                .await
                .unwrap(),
                delivered(true)
            );
            assert!(pending.is_pending());
        }
        assert_ne!(lease.lease_id, local.lease_id);
        clock.fetch_add(100, Ordering::SeqCst);
        channel.tick();
        assert_eq!(
            pending.try_result().unwrap().unwrap(),
            DialogAnswer::Answered("last".into())
        );
        assert!(store.list(&key).unwrap().is_empty());
        assert_eq!(
            respond(
                &state,
                &params("d", answered("duplicate".into()), JsValue::Undefined)
            )
            .await
            .unwrap(),
            delivered(false)
        );
        if let Some(server) = server {
            server.close().await;
        }
    }
}

struct FaultTranscript {
    native: crate::storage::JsonlTranscriptStore,
    fail: AtomicUsize,
}
impl crate::storage::TranscriptStore for FaultTranscript {
    fn append(
        &self,
        key: &crate::storage::TranscriptKey,
        entry: &JsValue,
    ) -> crate::storage::StoreResult<()> {
        let code = self.fail.load(Ordering::SeqCst);
        let kind = entry.get("type").unwrap().as_string().unwrap();
        if code == 1 || (code == 2 && kind.equals_str("turn_result")) {
            self.fail.store(0, Ordering::SeqCst);
            return Err(crate::storage::LedgerError::Io(std::io::Error::other(
                "injected transcript disk error",
            )));
        }
        crate::storage::TranscriptStore::append(&self.native, key, entry)
    }
    fn read(
        &self,
        key: &crate::storage::TranscriptKey,
        max: Option<u64>,
    ) -> crate::storage::StoreResult<crate::storage::TranscriptRead> {
        crate::storage::TranscriptStore::read(&self.native, key, max)
    }
}

#[tokio::test]
async fn user_dialog_respond_recovered_native_http_persists_once_retries_and_never_wakes_old_tool()
{
    for http in [false, true] {
        let scratch = Scratch::new();
        let backend = Arc::new(FaultTranscript {
            native: crate::storage::JsonlTranscriptStore::new(),
            fail: AtomicUsize::new(1),
        });
        let chat = scratch.0.join("chat");
        let adapter = crate::storage::AsyncTranscriptAdapter::new(
            backend.clone(),
            Arc::new(move |_| chat.clone()),
        );
        let storage = Arc::new(adapter.create_session_storage(
            crate::storage::SessionStorageInput {
                project_root: scratch.0.join("project"),
                pilot_home: scratch.0.join("home"),
                session_id: "s".into(),
                now: None,
            },
        ));
        let journal = Arc::new(
            create_gateway_user_dialog_journal(
                Some(&storage.transcript_path),
                JournalOptions::default(),
            )
            .unwrap(),
        );
        let store = Arc::new(
            FileGatewayUserDialogStore::new(StoreOptions::new(scratch.0.join("store"))).unwrap(),
        );
        let key = StoreKey {
            project_root: scratch.0.join("project").to_str().unwrap().into(),
            pilot_home: scratch.0.join("home").to_str().unwrap().into(),
            session_id: "s".into(),
        };
        let old = GatewayUserDialogs::default();
        let mut opts = ChannelOptions::new("s", old.bus.clone(), Arc::new(|_| Ok(())));
        opts.uuid = Arc::new(|| "d".into());
        opts.store = Some(store.clone());
        opts.store_key = Some(key.clone());
        opts.journal = Some(journal.clone());
        let channel = GatewayUserDialogChannel::new(opts);
        let mut pending = input(&channel);
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
        let ds = if let Some(server) = &server {
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
        let make_host = || {
            DialogPersistence::new(Some(ds.clone()), {
                let key = key.clone();
                let journal = journal.clone();
                Arc::new(move |input| {
                    assert!(input.get("sessionKey") == Some(&"s".into()));
                    Ok(Some(DialogSession {
                        key: key.clone(),
                        journal: Some(journal.clone()),
                    }))
                })
            })
            .with_recovery_storage({
                let storage = storage.clone();
                Arc::new(move |session| {
                    assert_eq!(session.key.session_id, JsString::from("s"));
                    Ok(storage.clone())
                })
            })
        };
        let state = GatewayUserDialogs {
            persistence: Some(Arc::new(make_host())),
            ..Default::default()
        };
        let p = params("d", answered("recovered".into()), JsValue::Undefined);
        let bad = params("d", answered(JsValue::Bool(false)), JsValue::Undefined);
        assert_eq!(
            respond(&state, &bad).await.unwrap_err().code,
            "INVALID_USER_DIALOG_RESPONSE"
        );
        assert!(
            respond(&state, &p)
                .await
                .unwrap_err()
                .message
                .to_utf8()
                .unwrap()
                .contains("disk error")
        );
        assert_eq!(journal.recover().unwrap().len(), 1);
        assert_eq!(store.list(&key).unwrap().len(), 1);
        assert!(pending.is_pending());
        backend.fail.store(2, Ordering::SeqCst);
        assert!(respond(&state, &p).await.is_err());
        assert_eq!(storage.read_transcript(None).unwrap().entries.len(), 1);
        assert!(!journal.recover().unwrap().is_empty());
        let (a, b) = tokio::join!(respond(&state, &p), respond(&state, &p));
        for result in [a, b] {
            assert_eq!(
                result.unwrap(),
                object([
                    ("delivered", JsValue::Bool(true)),
                    ("recovered", JsValue::Bool(true)),
                    ("reason", "gateway_restarted".into())
                ])
            );
        }
        let read = storage.read_transcript(None).unwrap();
        assert_eq!(read.entries.len(), 2);
        assert_eq!(read.entries[0].get("type"), Some(&"durable_message".into()));
        assert_eq!(read.entries[1].get("type"), Some(&"turn_result".into()));
        let message = read.entries[0].get("message").unwrap();
        assert_eq!(
            message.get("metadata").unwrap().get("purpose"),
            Some(&"gateway_user_dialog_recovery:d".into())
        );
        let JsValue::Array(content) = message.get("content").unwrap() else {
            panic!()
        };
        assert!(
            content[0]
                .get("text")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap()
                .ends_with("The user answered: recovered")
        );
        assert_eq!(
            read.entries[1].get("result").unwrap().get("stopReason"),
            Some(&"aborted_tools".into())
        );
        assert!(journal.recover().unwrap().is_empty());
        assert!(store.list(&key).unwrap().is_empty());
        assert!(pending.is_pending());
        assert!(pending.try_result().is_none());
        let restarted = GatewayUserDialogs {
            persistence: Some(Arc::new(make_host())),
            ..Default::default()
        };
        assert_eq!(respond(&restarted, &p).await.unwrap(), delivered(false));
        assert_eq!(storage.read_transcript(None).unwrap().entries.len(), 2);
        channel.abort(pending.request_id());
        if let Some(server) = server {
            server.close().await;
        }
    }
}

#[test]
fn user_dialog_respond_form_replays_actual_ts_legacy_and_draft_cases() {
    let cases = pilotdeck_protocol::parse_json(include_str!(
        "../../tests/fixtures/user-dialog-respond/form-cases.json"
    ))
    .unwrap();
    let JsValue::Array(cases) = &cases else {
        panic!()
    };
    for case in cases {
        assert_eq!(
            super::schema::accepts(case.get("schema").unwrap(), case.get("value").unwrap()),
            case.get("expected").unwrap().as_bool().unwrap(),
            "{}: {}",
            case.get("name")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
            stringify_json(case).unwrap().unwrap()
        );
    }
}
