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
async fn not_started(rx: &mut watch::Receiver<bool>) {
    assert!(
        !*rx.borrow_and_update(),
        "new same-key factory entered during delete"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(60), rx.wait_for(|v| *v))
            .await
            .is_err(),
        "same-key publication ticket was released before cleanup"
    );
}
#[tokio::test]
async fn close_then_retains_ticket_before_root_removal_with_absent_owner_and_cancelled_waiter() {
    for cancel in [false, true] {
        let ephemeral = Arc::new(EphemeralSessions::default());
        let key: JsString = "close-delete".into();
        let old = ephemeral.acquire(key.clone()).await.unwrap();
        std::fs::write(old.path().join("original"), b"OLD\n").unwrap();
        let (owner, mut started, created) = owner(ephemeral.clone());
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
        let next = owner.get_or_create(context(&key, None));
        not_started(&mut started).await;
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
        let fresh = created.lock().unwrap()[0].clone();
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
        let (owner, mut started, created) = owner(ephemeral.clone());
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
        let next = owner.get_or_create(context(&key, Some(root)));
        not_started(&mut started).await;
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
        let fresh = created.lock().unwrap()[0].clone();
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
