use super::*;
use crate::runtime::{project::DisposeFuture, session::LiveSession};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Barrier;

struct Factory {
    create_gate: Option<Arc<Barrier>>,
    dispose_gate: Option<Arc<Barrier>>,
    started: watch::Sender<usize>,
    disposed: watch::Sender<usize>,
    creates: AtomicUsize,
}
impl Factory {
    fn new(create_gate: Option<Arc<Barrier>>, dispose_gate: Option<Arc<Barrier>>) -> Arc<Self> {
        Arc::new(Self {
            create_gate,
            dispose_gate,
            started: watch::channel(0).0,
            disposed: watch::channel(0).0,
            creates: AtomicUsize::new(0),
        })
    }
    fn owner(self: &Arc<Self>) -> SessionOwner {
        let mut options = SessionOwnerOptions::new(self.clone());
        options.idle_sweep_interval_ms = 0;
        SessionOwner::new(options)
    }
}
struct Session {
    gate: Option<Arc<Barrier>>,
    disposed: watch::Sender<usize>,
}
impl LiveSession for Session {
    fn snapshot(&self) -> JsValue {
        JsValue::Undefined
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async move {
            self.disposed.send_modify(|n| *n += 1);
            if let Some(gate) = &self.gate {
                gate.wait().await;
            }
            Ok(())
        })
    }
}
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        let gate = self.create_gate.clone();
        let started = self.started.clone();
        let session = Arc::new(Session {
            gate: self.dispose_gate.clone(),
            disposed: self.disposed.clone(),
        });
        Box::pin(async move {
            started.send_modify(|n| *n += 1);
            if let Some(gate) = gate {
                gate.wait().await;
            }
            Ok(LiveSessionHandle::new(session, None))
        })
    }
}
fn context(key: &JsString) -> SessionContext {
    SessionContext {
        session_key: key.clone(),
        project_key: Some("project".into()),
        channel_key: "web".into(),
        allowed_tools: None,
        disallowed_tools: None,
    }
}
async fn entered(sender: &watch::Sender<usize>) {
    tokio::time::timeout(
        Duration::from_secs(2),
        sender.subscribe().wait_for(|n| *n > 0),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn archive_blocks_same_key_writers_but_preserves_seed_maintenance_contract() {
    fn is_send<T: Send>() {}
    is_send::<ArchiveReservation>();
    let factory = Factory::new(None, None);
    let owner = factory.owner();
    let key: JsString = "archive".into();
    let guard = owner.try_reserve_archive(&key, true).unwrap().unwrap();
    assert!(owner.try_reserve_archive(&key, false).unwrap().is_none());
    assert!(owner.begin_turn(&key, &"run".into()).is_none());
    assert!(owner.get_or_create(context(&key)).await.is_err());
    let error = owner.close(&key).await.unwrap_err();
    assert_eq!(
        error.js_error().and_then(|error| error.code.as_deref()),
        Some("SESSION_BUSY")
    );
    assert!(owner.begin_maintenance(&key).unwrap().is_none());
    assert_eq!(factory.creates.load(Ordering::SeqCst), 0);
    owner.get_or_create(context(&"other".into())).await.unwrap();
    drop(guard);
    owner.get_or_create(context(&key)).await.unwrap();
    assert!(
        owner.try_reserve_archive(&key, true).unwrap().is_none(),
        "restore rejects cached session"
    );
    let old_handle = owner.current_handle(&key).unwrap();
    let active = old_handle.begin_activity().unwrap();
    assert!(
        owner.try_reserve_archive(&key, false).unwrap().is_none(),
        "direct activity is busy"
    );
    drop(active);
    let read = owner.try_reserve_archive(&key, false).unwrap().unwrap();
    assert!(
        old_handle.begin_activity().is_err(),
        "old handle cannot start body during archive"
    );
    drop(read);
    drop(old_handle.begin_activity().unwrap());
    let seed = owner.begin_maintenance(&key).unwrap().unwrap();
    assert!(owner.try_reserve_archive(&key, false).unwrap().is_none());
    owner.get_or_create(context(&key)).await.unwrap();
    drop(seed);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn unpublished_creation_and_inflight_close_prevent_archives() {
    let create = Arc::new(Barrier::new(2));
    let dispose = Arc::new(Barrier::new(2));
    let factory = Factory::new(Some(create.clone()), Some(dispose.clone()));
    let owner = factory.owner();
    let key: JsString = "pending".into();
    let future = owner.get_or_create(context(&key));
    entered(&factory.started).await;
    assert!(owner.try_reserve_archive(&key, false).unwrap().is_none());
    create.wait().await;
    future.await.unwrap();
    let close = owner.close(&key);
    entered(&factory.disposed).await;
    assert!(owner.try_reserve_archive(&key, false).unwrap().is_none());
    assert!(owner.try_reserve_archive(&key, true).unwrap().is_none());
    dispose.wait().await;
    close.await.unwrap();
    drop(owner.try_reserve_archive(&key, true).unwrap().unwrap());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn turn_admission_and_live_completion_exclude_archive() {
    let factory = Factory::new(None, None);
    let owner = factory.owner();
    let key: JsString = "turn".into();
    let run: JsString = "run".into();
    let active = owner.begin_turn(&key, &run).unwrap();
    let completion = owner.begin_completion(&key, &run);
    assert!(owner.try_reserve_archive(&key, false).unwrap().is_none());
    owner.end_turn(&key, Some(&run), Some(&active));
    assert!(
        owner.try_reserve_archive(&key, false).unwrap().is_none(),
        "completion cleanup still owns key"
    );
    owner.complete_turn(&key, &completion);
    drop(owner.try_reserve_archive(&key, false).unwrap().unwrap());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_waiter_keeps_blocking_guard_and_shutdown_waits_for_real_io() {
    let factory = Factory::new(None, None);
    let owner = factory.owner();
    let key: JsString = "blocking".into();
    owner.get_or_create(context(&key)).await.unwrap();
    let guard = owner.try_reserve_archive(&key, false).unwrap().unwrap();
    let gate = BlockingGate::new();
    let _release_on_unwind = ReleaseBlocking(gate.clone());
    let job_gate = gate.clone();
    let (started, entered) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            started.send(()).unwrap();
            job_gate.wait();
        })
        .await
        .unwrap();
    });
    entered.await.unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(owner.try_reserve_archive(&key, false).unwrap().is_none());
    assert!(owner.close(&key).await.is_err());
    let mut shutdown = tokio::spawn(owner.shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut shutdown)
            .await
            .is_err()
    );
    assert_eq!(
        *factory.disposed.borrow(),
        0,
        "shutdown must not dispose writer before archive IO ends"
    );
    assert!(
        owner.try_reserve_archive(&key, false).is_err(),
        "stopped owner rejects new archive"
    );
    gate.release();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(*factory.disposed.borrow(), 1);
}

#[tokio::test]
async fn panic_in_blocking_archive_releases_reservation_and_pending_fence() {
    let factory = Factory::new(None, None);
    let owner = factory.owner();
    let key: JsString = "panic".into();
    let guard = owner.try_reserve_archive(&key, true).unwrap().unwrap();
    let panic = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        panic!("archive fixture IO panic");
    })
    .await
    .unwrap_err();
    assert!(panic.is_panic());
    drop(owner.try_reserve_archive(&key, true).unwrap().unwrap());
    owner.get_or_create(context(&key)).await.unwrap();
    owner.shutdown().await.unwrap();
}

struct BlockingGate {
    ready: Mutex<bool>,
    changed: std::sync::Condvar,
}
impl BlockingGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            ready: Mutex::new(false),
            changed: std::sync::Condvar::new(),
        })
    }
    fn release(&self) {
        *self
            .ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.changed.notify_all();
    }
    fn wait(&self) {
        let _ready = self
            .changed
            .wait_while(self.ready.lock().unwrap(), |ready| !*ready)
            .unwrap();
    }
}
struct ReleaseBlocking(Arc<BlockingGate>);
impl Drop for ReleaseBlocking {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[tokio::test]
async fn paused_project_refuses_archive_before_and_after_cached_writer_drain() {
    let factory = Factory::new(None, None);
    let owner = factory.owner();
    let key: JsString = "paused".into();
    owner.get_or_create(context(&key)).await.unwrap();
    let guard = owner.try_reserve_archive(&key, false).unwrap().unwrap();
    let closing = owner.close_project(&"project".into());
    assert!(
        owner.try_reserve_archive(&key, false).is_err(),
        "project pause wins before busy classification"
    );
    drop(guard);
    closing.await.unwrap();
    let _scope = owner
        .reserve_project(&key, Some(&"project".into()), &"project".into())
        .unwrap();
    assert!(
        owner.try_reserve_archive(&key, true).is_err(),
        "cold key with scoped paused project is refused"
    );
    owner.resume_project(&"project".into());
    drop(owner.try_reserve_archive(&key, true).unwrap().unwrap());
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_cold_project_archive_blocks_only_its_project_close_until_real_io_finishes() {
    let factory = Factory::new(None, None);
    let owner = factory.owner();
    let key: JsString = "cold-project-archive".into();
    let project: JsString = "selected-project".into();
    assert!(owner.current_handle(&key).is_none());
    let guard = owner
        .try_reserve_archive_with_project(&key, &project, true)
        .unwrap()
        .unwrap();
    let gate = BlockingGate::new();
    let _release_on_unwind = ReleaseBlocking(gate.clone());
    let job_gate = gate.clone();
    let (started, entered) = tokio::sync::oneshot::channel();
    let io = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let _ = started.send(());
        job_gate.wait();
    });
    entered.await.unwrap();
    let mut closing = tokio::spawn(owner.close_project(&project));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut closing)
            .await
            .is_err()
    );
    assert!(
        owner
            .try_reserve_archive_with_project(&"other-cold-key".into(), &project, true)
            .is_err(),
        "project pause rejects a new cold archive without any record/scope"
    );
    tokio::time::timeout(
        Duration::from_secs(2),
        owner.close_project(&"other-project".into()),
    )
    .await
    .unwrap()
    .unwrap();
    gate.release();
    io.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), closing)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert!(
        owner
            .try_reserve_archive_with_project(&key, &project, true)
            .is_err()
    );
    owner.resume_project(&project);
    drop(
        owner
            .try_reserve_archive_with_project(&key, &project, true)
            .unwrap()
            .unwrap(),
    );
    owner.shutdown().await.unwrap();
}
