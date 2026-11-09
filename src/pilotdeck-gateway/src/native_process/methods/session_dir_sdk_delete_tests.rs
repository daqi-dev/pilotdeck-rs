//! Barrier-controlled real owner + real temporary-directory deletion consumers.
use super::*;
use crate::{
    native_process::sdk_ephemeral::{EphemeralSessions, TemporaryRoot},
    runtime::{
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        refresh::BoxFuture,
        session::{
            LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwner,
            SessionOwnerOptions,
        },
    },
};
use std::{path::PathBuf, sync::Mutex, time::Duration};
use tokio::sync::watch;
struct Session {
    _root: Arc<TemporaryRoot>,
}
impl LiveSession for Session {
    fn snapshot(&self) -> JsValue {
        object([("messages", JsValue::Array(vec![]))])
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
struct Factory {
    ephemeral: Arc<EphemeralSessions>,
    started: watch::Sender<bool>,
    created: Arc<Mutex<Vec<PathBuf>>>,
}
impl SessionFactory for Factory {
    fn create(
        &self,
        context: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let roots = self.ephemeral.clone();
        let started = self.started.clone();
        let created = self.created.clone();
        Box::pin(async move {
            started.send_replace(true);
            let root = roots.acquire(context.session_key).await?;
            std::fs::write(
                root.path().join("new-session-input"),
                b"FRESH-WRITER-BYTES\n",
            )
            .map_err(|e| RuntimeError::new(e.to_string()))?;
            created.lock().unwrap().push(root.path().to_owned());
            Ok(LiveSessionHandle::new(
                Arc::new(Session { _root: root }),
                None,
            ))
        })
    }
}
fn owner(
    ephemeral: Arc<EphemeralSessions>,
) -> (
    SessionOwner,
    watch::Receiver<bool>,
    Arc<Mutex<Vec<PathBuf>>>,
) {
    let (started, rx) = watch::channel(false);
    let created = Arc::new(Mutex::new(Vec::new()));
    let mut options = SessionOwnerOptions::new(Arc::new(Factory {
        ephemeral,
        started,
        created: created.clone(),
    }));
    options.idle_sweep_interval_ms = 0;
    (SessionOwner::new(options), rx, created)
}
fn context(key: &JsString, project: Option<JsString>) -> SessionContext {
    SessionContext {
        session_key: key.clone(),
        project_key: project,
        channel_key: "cli".into(),
        allowed_tools: None,
        disallowed_tools: None,
    }
}
async fn changed(rx: &mut watch::Receiver<bool>) {
    tokio::time::timeout(Duration::from_secs(2), rx.wait_for(|v| *v))
        .await
        .expect("barrier entered")
        .expect("barrier sender alive");
}
fn cleanup_ticket_still_owned(ticket: &watch::Receiver<bool>, started: &watch::Receiver<bool>) {
    assert!(
        !*ticket.borrow(),
        "actual cleanup publication ticket released at the barrier"
    );
    // Supplemental signal snapshot; the real held publication ticket above
    // is the deterministic barrier, not an elapsed-time absence observation.
    assert!(!*started.borrow(), "new same-key factory entered during delete");
}
#[tokio::test]
async fn close_then_retains_ticket_before_root_removal_with_absent_owner_and_cancelled_waiter() {
    for cancel in [false, true] {
        let ephemeral = Arc::new(EphemeralSessions::default());
        let key: JsString = "close-delete".into();
        let old = ephemeral.acquire(key.clone()).await.unwrap();
        std::fs::write(old.path().join("original"), b"OLD\n").unwrap();
        let (owner, started, created) = owner(ephemeral.clone());
        let (entered, mut inside) = watch::channel(false);
        let (release, mut gate) = watch::channel(false);
        let roots = ephemeral.clone();
        let delete_key = key.clone();
        let closing = owner.close_then(&key, move || {
            Box::pin(async move {
                entered.send_replace(true);
                let _ = gate.wait_for(|v| *v).await;
                assert!(roots.delete(delete_key).await?);
                Ok(())
            })
        });
        changed(&mut inside).await;
        assert!(
            old.path().exists(),
            "old root still exists at barrier before actual deletion"
        );
        let closing = if cancel {
            drop(closing);
            None
        } else {
            Some(closing)
        };
        let delete_ticket = owner.publication_ticket_probe(&key);
        let next = owner.get_or_create(context(&key, None));
        cleanup_ticket_still_owned(&delete_ticket, &started);
        release.send_replace(true);
        if let Some(closing) = closing {
            tokio::time::timeout(Duration::from_secs(2), closing)
                .await
                .unwrap()
                .unwrap();
        }
        let handle = tokio::time::timeout(Duration::from_secs(2), next)
            .await
            .unwrap()
            .unwrap();
        let created = created.lock().unwrap().clone();
        assert_eq!(created.len(), 1, "one same-key session actually published");
        let fresh = created[0].clone();
        assert_ne!(fresh, old.path());
        assert!(!old.path().exists());
        assert_eq!(
            std::fs::read(fresh.join("new-session-input")).unwrap(),
            b"FRESH-WRITER-BYTES\n"
        );
        drop(handle);
        owner.shutdown().await.unwrap();
        ephemeral.close().await.unwrap();
        assert!(!fresh.exists());
    }
}
#[tokio::test]
async fn close_then_callback_error_preserves_original_bytes_and_releases_the_ticket() {
    let ephemeral = Arc::new(EphemeralSessions::default());
    let key: JsString = "failed-cleanup".into();
    let root = ephemeral.acquire(key.clone()).await.unwrap();
    std::fs::write(root.path().join("original"), b"KEEP\n").unwrap();
    let (owner, _, created) = owner(ephemeral.clone());
    let error = owner
        .close_then(&key, || {
            Box::pin(async { Err(RuntimeError::new("OWNED_CLEANUP_FAILED")) })
        })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "OWNED_CLEANUP_FAILED");
    assert_eq!(
        std::fs::read(root.path().join("original")).unwrap(),
        b"KEEP\n"
    );
    let handle = tokio::time::timeout(
        Duration::from_secs(2),
        owner.get_or_create(context(&key, None)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(created.lock().unwrap()[0], root.path());
    drop(handle);
    owner.shutdown().await.unwrap();
    ephemeral.close().await.unwrap();
}
#[tokio::test]
async fn actual_sdk_session_delete_holds_publication_until_sdk_cleanup_even_if_caller_is_cancelled()
{
    for cancel in [false, true] {
        let (_scratch, mut requests) = crate::native_process::management_tests::setup();
        requests.owner.shutdown().await.unwrap();
        let ephemeral = Arc::new(EphemeralSessions::default());
        let key: JsString = "delete-consumer".into();
        let old = ephemeral.acquire(key.clone()).await.unwrap();
        std::fs::write(old.path().join("original"), b"DELETE\n").unwrap();
        let (owner, started, created) = owner(ephemeral.clone());
        requests.owner = owner.clone();
        requests.ephemeral = Some(ephemeral.clone());
        let (entered, mut inside) = watch::channel(false);
        let (release, gate) = watch::channel(false);
        requests.clear_sdk_state = Some(Arc::new(move |actual| {
            assert_eq!(actual, JsString::from("delete-consumer"));
            let entered = entered.clone();
            let mut gate = gate.clone();
            Box::pin(async move {
                entered.send_replace(true);
                let _ = gate.wait_for(|v| *v).await;
                Ok(())
            })
        }));
        let requests = Arc::new(requests);
        let request = requests.clone();
        let delete_key = key.clone();
        let task = tokio::spawn(async move {
            request
                .session_delete(&object([("sessionKey", JsValue::String(delete_key))]))
                .await
        });
        changed(&mut inside).await;
        assert!(
            !old.path().exists(),
            "real ephemeral delete happened before SDK cleanup barrier"
        );
        if cancel {
            task.abort();
        }
        let root: JsString = requests
            .options
            .project
            .to_string_lossy()
            .into_owned()
            .into();
        let delete_ticket = owner.publication_ticket_probe(&key);
        let next = owner.get_or_create(context(&key, Some(root)));
        cleanup_ticket_still_owned(&delete_ticket, &started);
        release.send_replace(true);
        if cancel {
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        let handle = tokio::time::timeout(Duration::from_secs(2), next)
            .await
            .unwrap()
            .unwrap();
        let created = created.lock().unwrap().clone();
        assert_eq!(created.len(), 1, "one same-key session actually published");
        let fresh = created[0].clone();
        assert_ne!(fresh, old.path());
        assert_eq!(
            std::fs::read(fresh.join("new-session-input")).unwrap(),
            b"FRESH-WRITER-BYTES\n"
        );
        drop(handle);
        owner.shutdown().await.unwrap();
        ephemeral.close().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn plain_close_receives_eviction_success_while_sdk_cleanup_fails_independently() {
    let ephemeral = Arc::new(EphemeralSessions::default());
    let (owner, _, _) = owner(ephemeral.clone());
    let key: JsString = "close-independent".into();
    let (entered, mut inside) = watch::channel(false);
    let (release, mut gate) = watch::channel(false);
    let cleanup = owner.close_then(&key, move || {
        Box::pin(async move {
            entered.send_replace(true);
            gate.wait_for(|value| *value).await.unwrap();
            Err(RuntimeError::new("SDK-CLEANUP-FAILED"))
        })
    });
    changed(&mut inside).await;
    let ticket = owner.publication_ticket_probe(&key);
    assert!(!*ticket.borrow());
    // Completion must arrive while the callback is still blocked, and carries
    // only the eviction outcome. Always release the task before assertions.
    let closed = tokio::time::timeout(Duration::from_secs(2), owner.close(&key)).await;
    release.send_replace(true);
    let cleaned = cleanup.await;
    assert!(
        closed
            .expect("ordinary close waited for SDK callback")
            .is_ok()
    );
    assert_eq!(cleaned.unwrap_err().to_string(), "SDK-CLEANUP-FAILED");
    assert!(*ticket.borrow());
    owner.shutdown().await.unwrap();
    ephemeral.close().await.unwrap();
}

struct EvictionFailure {
    entered: watch::Sender<bool>,
    release: watch::Receiver<bool>,
}
impl LiveSession for EvictionFailure {
    fn snapshot(&self) -> JsValue {
        object([("messages", JsValue::Array(vec![]))])
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        let entered = self.entered.clone();
        let mut release = self.release.clone();
        Box::pin(async move {
            entered.send_replace(true);
            release.wait_for(|value| *value).await.unwrap();
            Err(RuntimeError::new("EVICTION-FAILED"))
        })
    }
}
struct EvictionFactory(Arc<EvictionFailure>);
impl SessionFactory for EvictionFactory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let session = self.0.clone();
        Box::pin(async move { Ok(LiveSessionHandle::new(session, None)) })
    }
}
#[tokio::test]
async fn sdk_delete_waits_for_failed_inflight_close_and_preserves_temporary_root() {
    let (_scratch, mut requests) = crate::native_process::management_tests::setup();
    requests.owner.shutdown().await.unwrap();
    let roots = Arc::new(EphemeralSessions::default());
    let key: JsString = "failed-existing-close".into();
    let root = roots.acquire(key.clone()).await.unwrap();
    std::fs::write(root.path().join("keep"), b"WRITER-NOT-QUIESCENT").unwrap();
    let (entered, mut inside) = watch::channel(false);
    let (release, gate) = watch::channel(false);
    let mut options =
        SessionOwnerOptions::new(Arc::new(EvictionFactory(Arc::new(EvictionFailure {
            entered,
            release: gate,
        }))));
    options.idle_sweep_interval_ms = 0;
    let owner = SessionOwner::new(options);
    let project: JsString = requests.options.project.to_string_lossy().as_ref().into();
    let handle = owner
        .get_or_create(context(&key, Some(project)))
        .await
        .unwrap();
    drop(handle);
    requests.owner = owner.clone();
    requests.ephemeral = Some(roots.clone());
    let closing = owner.close(&key);
    changed(&mut inside).await;
    let original_ticket = owner.publication_ticket_probe(&key);
    let requests = Arc::new(requests);
    let cloned = requests.clone();
    let delete_key = key.clone();
    let deletion = tokio::spawn(async move {
        cloned
            .session_delete(&object([("sessionKey", JsValue::String(delete_key))]))
            .await
    });
    // The original eviction is blocked. Release it with its real error;
    // deletion must propagate that error, never run root removal afterward.
    let registered = tokio::time::timeout(Duration::from_secs(2), async {
        while owner
            .publication_ticket_probe(&key)
            .same_channel(&original_ticket)
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    release.send_replace(true);
    let closed = closing.await;
    let deleted = deletion.await.unwrap();
    let preserved = std::fs::read(root.path().join("keep"));
    let stopped = owner.shutdown().await;
    let cleaned = roots.close().await;
    let disposed = requests.project.dispose().await;
    assert!(
        registered.is_ok(),
        "actual delete reserved its ticket behind the blocked eviction"
    );
    let closed = closed.expect_err("blocked real eviction must fail");
    // LiveSessionHandle intentionally preserves the two lifecycle aggregates;
    // Display prints only the outer message. Inspect the structured source,
    // then require delete to transmit that exact public close message.
    assert_eq!(
        closed.message, "Failed to dispose agent handle.",
        "{closed:?}"
    );
    assert_eq!(closed.causes.len(), 1, "{closed:?}");
    assert_eq!(
        closed.causes[0].message, "Failed to dispose agent session.",
        "{closed:?}"
    );
    assert_eq!(closed.causes[0].causes.len(), 1, "{closed:?}");
    assert_eq!(
        closed.causes[0].causes[0].message, "EVICTION-FAILED",
        "{closed:?}"
    );
    assert!(closed.causes[0].causes[0].causes.is_empty(), "{closed:?}");
    let deleted = deleted.expect_err("delete must transmit the blocked eviction failure");
    assert_eq!(deleted.code, "gateway_request_failed");
    assert_eq!(
        deleted.message,
        JsString::from(closed.message.as_str()),
        "{deleted:?}"
    );
    assert_eq!(preserved.unwrap(), b"WRITER-NOT-QUIESCENT");
    stopped.unwrap();
    cleaned.unwrap();
    disposed.unwrap();
}

#[tokio::test]
async fn repeated_sdk_deletes_and_clear_failure_retry_preserve_durable_namesake() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for clear_fails in [false, true] {
        let (_scratch, mut requests) = crate::native_process::management_tests::setup();
        let key: JsString = "duplicate-delete".into();
        let (chat, permit) = resolve_chat_dir(
            requests.options.home.clone(),
            requests.options.project.clone(),
        )
        .await
        .unwrap();
        std::fs::create_dir_all(&chat).unwrap();
        let durable = chat.join("duplicate-delete.jsonl");
        let recorder = crate::storage::recorder::Recorder::new(
            Arc::new(JsonlTranscriptStore::new_bounded()),
            crate::storage::TranscriptKey {
                project_root: requests.options.project.clone(),
                pilot_home: requests.options.home.clone(),
                session_id: key.clone(),
                transcript_path: durable.clone(),
            },
        );
        recorder
            .start_turn(&key, &"durable-namesake".into())
            .unwrap();
        drop(recorder);
        let durable_bytes = std::fs::read(&durable).unwrap();
        drop(permit);
        let roots = Arc::new(EphemeralSessions::default());
        let temporary = roots.acquire(key.clone()).await.unwrap();
        requests.ephemeral = Some(roots.clone());
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        requests.clear_sdk_state = Some(Arc::new(move |_| {
            let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
            Box::pin(async move {
                if clear_fails && first {
                    Err(RequestError::new("CLEAR-FAILURE"))
                } else {
                    Ok(())
                }
            })
        }));
        let params = object([("sessionKey", JsValue::String(key.clone()))]);
        let first = requests.session_delete(&params).await;
        assert_eq!(first.is_err(), clear_fails);
        requests.session_delete(&params).await.unwrap();
        requests.session_delete(&params).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert!(!temporary.path().exists());
        assert_eq!(std::fs::read(&durable).unwrap(), durable_bytes);
        // A genuinely opened new durable recording clears only this prior
        // temporary identity. Repeating delete cannot mask it forever.
        let lease = Arc::new(
            requests
                .project
                .acquire(Some(&requests.options.project))
                .unwrap(),
        );
        let storage =
            roots.durable_storage(crate::native_process::project_storage::for_generation(
                requests.options.home.clone(),
                lease,
            ));
        let result = storage(context(
            &key,
            Some(requests.options.project.to_string_lossy().as_ref().into()),
        ))
        .await
        .unwrap();
        let crate::runtime::session::factory::SessionStorage::Recording(plan) = result else {
            panic!("real durable recording plan")
        };
        let opened = (plan.open)().unwrap();
        drop(opened);
        drop(plan);
        drop(storage);
        requests.session_delete(&params).await.unwrap();
        assert!(
            !durable.exists(),
            "explicit new durable admission must retire the temporary tombstone"
        );
        roots.close().await.unwrap();
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_sdk_deletes_are_idempotent_and_do_not_remove_durable_namesake() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (_scratch, mut requests) = crate::native_process::management_tests::setup();
    let key: JsString = "concurrent-delete".into();
    let (chat, permit) = resolve_chat_dir(
        requests.options.home.clone(),
        requests.options.project.clone(),
    )
    .await
    .unwrap();
    std::fs::create_dir_all(&chat).unwrap();
    let durable = chat.join("concurrent-delete.jsonl");
    std::fs::write(&durable, b"PRESERVE-CONCURRENT-NAMESAKE\n").unwrap();
    drop(permit);
    let roots = Arc::new(EphemeralSessions::default());
    let temporary = roots.acquire(key.clone()).await.unwrap();
    requests.ephemeral = Some(roots.clone());
    let (entered, mut inside) = watch::channel(false);
    let (release, gate) = watch::channel(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    requests.clear_sdk_state = Some(Arc::new(move |_| {
        let first = count.fetch_add(1, Ordering::SeqCst) == 0;
        let entered = entered.clone();
        let mut gate = gate.clone();
        Box::pin(async move {
            if first {
                entered.send_replace(true);
                gate.wait_for(|value| *value).await.unwrap();
            }
            Ok(())
        })
    }));
    let requests = Arc::new(requests);
    let first_request = requests.clone();
    let first_key = key.clone();
    let first = tokio::spawn(async move {
        first_request
            .session_delete(&object([("sessionKey", JsValue::String(first_key))]))
            .await
    });
    changed(&mut inside).await;
    let first_ticket = requests.owner.publication_ticket_probe(&key);
    let second_request = requests.clone();
    let second_key = key.clone();
    let second = tokio::spawn(async move {
        second_request
            .session_delete(&object([("sessionKey", JsValue::String(second_key))]))
            .await
    });
    let queued = tokio::time::timeout(Duration::from_secs(2), async {
        while requests
            .owner
            .publication_ticket_probe(&key)
            .same_channel(&first_ticket)
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let held = !*first_ticket.borrow();
    release.send_replace(true);
    let one = first.await.unwrap();
    let two = second.await.unwrap();
    assert!(
        queued.is_ok(),
        "second real delete reserved its same-key ticket"
    );
    assert!(
        held,
        "first cleanup remained owned while second deletion queued"
    );
    one.unwrap();
    two.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!temporary.path().exists());
    assert_eq!(
        std::fs::read(&durable).unwrap(),
        b"PRESERVE-CONCURRENT-NAMESAKE\n"
    );
    requests.owner.shutdown().await.unwrap();
    roots.close().await.unwrap();
    requests.project.dispose().await.unwrap();
}
