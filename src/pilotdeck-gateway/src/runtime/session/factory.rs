//! ProjectSessionFactory: prepare resources, restore one writer, then transfer
//! them to the real AgentSession adapter. The project registry still owns
//! generations; the session owner still serializes publication by key.
use super::{LiveSession, LiveSessionHandle, LiveTurn, SessionContext, SessionFactory};
use crate::{
    host::modules::tool_context::{HostToolCheckpoint, SeedState},
    runtime::{
        cleanup_task::lock,
        mcp::registry::SessionMcpRuntimeRegistry,
        project::{DisposeFuture, RuntimeError, RuntimeLease, RuntimeResult},
        refresh::BoxFuture,
        turn::{
            host_factory::TurnHostFactory,
            live::LiveSessionAdapter,
            runner::tail::{SessionTail, TailConfig},
        },
    },
    storage::{
        Diagnostic, TranscriptKey, TranscriptRead, TranscriptStore, WriterState, recorder::Recorder,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

/// AgentSession.ts:311-320 / AgentSessionState.ts:25-32. A new adapter supplies
/// idle status, no currentTurnId and a fresh cancellation controller.
#[derive(Clone, Debug)]
pub struct ReloadSnapshot {
    pub session_id: JsString,
    pub state: DurableSessionState,
    pub cwd: PathBuf,
    pub transcript_path: PathBuf,
    pub file_state: SeedState,
    pub transcript_writer_state: WriterState,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DurableSessionState {
    pub messages: Vec<JsValue>,
    pub usage: JsObject,
    pub permission_denials: Vec<JsValue>,
    pub metadata: JsObject,
}

/// Required sources, rather than invented empty state. For stdio, file_state
/// reads the session's saved seed (agentLoopSidecarClient.ts:297,415,422), not
/// a different in-progress turn's tool checkpoint.
#[derive(Clone)]
pub struct ReloadSource {
    pub session_id: JsString,
    pub cwd: PathBuf,
    pub transcript_path: PathBuf,
    pub projection: Arc<SessionProjection>,
    pub file_state: Arc<dyn Fn() -> Result<SeedState, RuntimeError> + Send + Sync>,
}
impl ReloadSource {
    pub fn snapshot_consistent(&self, recorder: &Recorder) -> Result<ReloadSnapshot, RuntimeError> {
        // A synchronous TS snapshot cannot interleave an append. Avoid taking
        // projection -> writer locks (append takes them in the reverse order).
        for _ in 0..8 {
            let before = recorder.writer().snapshot_state();
            let snapshot = self.snapshot(before.clone());
            let after = recorder.writer().snapshot_state();
            if before.sequence == after.sequence && before.last_entry_id == after.last_entry_id {
                return snapshot;
            }
        }
        Err(RuntimeError::new(
            "Session changed during runtime reload; retry.",
        ))
    }
    fn snapshot(&self, writer: WriterState) -> Result<ReloadSnapshot, RuntimeError> {
        Ok(ReloadSnapshot {
            session_id: self.session_id.clone(),
            state: self.projection.snapshot()?,
            cwd: self.cwd.clone(),
            transcript_path: self.transcript_path.clone(),
            file_state: (self.file_state)()?,
            transcript_writer_state: writer,
        })
    }
}

/// Durable state is restored from storage only after the predecessor writer
/// has closed. A pre-drain snapshot must never rewind that durable sequence.
#[derive(Default)]
pub struct SessionProjection {
    entries: Mutex<Vec<JsValue>>,
    last_valid: Mutex<DurableSessionState>,
}
impl SessionProjection {
    pub fn snapshot(&self) -> Result<DurableSessionState, RuntimeError> {
        project(&lock(&self.entries))
    }
    fn restore(&self, entries: Vec<JsValue>) -> RuntimeResult {
        let state = project(&entries)?;
        *lock(&self.entries) = entries;
        *lock(&self.last_valid) = state;
        Ok(())
    }
}

// Shared across factory instances. Weak entries do not retain idle sessions.
// The advisory ledger lock also excludes writers in another process.
type WriterMutex = tokio::sync::Mutex<()>;
struct WriterLease {
    // Unlock the file before waking the next in-process waiter.
    _file: crate::storage::LedgerLock,
    _process: tokio::sync::OwnedMutexGuard<()>,
}
type SharedWriterLease = Arc<Mutex<Option<WriterLease>>>;
/// Cleanup hooks may still append. Retire new writes on unwind, then keep the
/// lease until admitted appends drain. A provider's append may depend on the
/// runtime making progress, so Drop must not wait for its commit lock here.
struct WriterCloseGuard {
    writer: Arc<crate::storage::TranscriptWriter>,
    _lease: Option<WriterLease>,
}
impl Drop for WriterCloseGuard {
    fn drop(&mut self) {
        self.writer.begin_close();
        let writer = self.writer.clone();
        let lease = self._lease.take();
        let close = move || {
            writer.close();
            drop(lease);
        };
        // An independent blocking thread also works when a future is dropped
        // after its runtime has exited. It owns the lease through the drain.
        std::thread::spawn(close);
    }
}
async fn acquire_writer(
    path: PathBuf,
    deadline: tokio::time::Instant,
) -> Result<WriterLease, RuntimeError> {
    acquire_writer_resolved(
        async move {
            tokio::task::spawn_blocking(move || crate::storage::canonical_transcript_path(&path))
                .await
                .map_err(|e| RuntimeError::new(e.to_string()))?
                .map_err(|e| RuntimeError::new(e.to_string()))
        },
        deadline,
    )
    .await
}

async fn acquire_writer_resolved(
    resolved: impl std::future::Future<Output = Result<PathBuf, RuntimeError>>,
    deadline: tokio::time::Instant,
) -> Result<WriterLease, RuntimeError> {
    static WRITERS: OnceLock<Mutex<HashMap<PathBuf, Weak<WriterMutex>>>> = OnceLock::new();
    // Path resolution is not a cancellable busy-lock wait. Its elapsed time
    // consumes the remaining wait budget; after expiry only an idle try remains.
    let path = resolved.await?;
    let mutex = {
        let mut writers = lock(WRITERS.get_or_init(Mutex::default));
        writers.retain(|_, entry| entry.strong_count() != 0);
        writers
            .entry(path.clone())
            .or_default()
            .upgrade()
            .unwrap_or_else(|| {
                let mutex = Arc::new(WriterMutex::new(()));
                writers.insert(path.clone(), Arc::downgrade(&mutex));
                mutex
            })
    };
    let process = acquire_process_writer(mutex, deadline).await?;
    let file = tokio::task::spawn_blocking(move || crate::storage::LedgerLock::acquire(&path))
        .await
        .map_err(|e| RuntimeError::new(e.to_string()))?
        .map_err(|e| match e {
            crate::storage::LedgerError::Io(ref io)
                if io.kind() == std::io::ErrorKind::WouldBlock =>
            {
                writer_busy()
            }
            _ => RuntimeError::new(e.to_string()),
        })?
        .1;
    Ok(WriterLease {
        _process: process,
        _file: file,
    })
}
async fn acquire_process_writer(
    mutex: Arc<WriterMutex>,
    deadline: tokio::time::Instant,
) -> Result<tokio::sync::OwnedMutexGuard<()>, RuntimeError> {
    if deadline <= tokio::time::Instant::now() {
        // An exhausted drain budget still allows an idle writer, never a wait.
        mutex.try_lock_owned().map_err(|_| writer_busy())
    } else {
        tokio::time::timeout_at(deadline, mutex.lock_owned())
            .await
            .map_err(|_| writer_busy())
    }
}
fn writer_busy() -> RuntimeError {
    let message = "Session writer is still draining; retry session recreation.";
    RuntimeError::new(message).with_js_error(crate::runtime::project::RuntimeJsError {
        name: "Error".into(),
        code: Some("SESSION_WRITER_BUSY".into()),
        message: message.into(),
        requires_host: false,
    })
}

struct ProjectedStore {
    _writer_lease: SharedWriterLease,
    inner: Arc<dyn TranscriptStore>,
    key: TranscriptKey,
    projection: Arc<SessionProjection>,
}
impl TranscriptStore for ProjectedStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> crate::storage::StoreResult<()> {
        if key != &self.key {
            return self.inner.append(key, entry);
        }
        let mut entries = lock(&self.projection.entries);
        let mut candidate = entries.clone();
        candidate.push(entry.clone());
        self.inner.append(key, entry)?;
        *entries = candidate;
        // Projection failure cannot undo a committed durable entry. Fallible
        // reload/submit surfaces it; the ordinary infallible view keeps its
        // last successfully projected state.
        if let Ok(state) = project(&entries) {
            *lock(&self.projection.last_valid) = state;
        }
        Ok(())
    }
    fn read(
        &self,
        key: &TranscriptKey,
        max_bytes: Option<u64>,
    ) -> crate::storage::StoreResult<TranscriptRead> {
        self.inner.read(key, max_bytes)
    }
}

pub type Release = Box<dyn FnOnce() -> BoxFuture<RuntimeResult> + Send>;
/// Register immediately after every acquisition, before the next await.
/// Explicit release and cancellation both consume the same reverse-order list.
#[derive(Default)]
pub struct PreparedResources {
    releases: Vec<Release>,
}
impl PreparedResources {
    pub fn push(&mut self, release: impl FnOnce() -> BoxFuture<RuntimeResult> + Send + 'static) {
        self.releases.push(Box::new(release));
    }
    pub fn retain_lease(&mut self, mut lease: RuntimeLease) {
        self.push(move || Box::pin(async move { lease.release().await }));
    }
    pub async fn release(mut self) -> RuntimeResult {
        release_all(std::mem::take(&mut self.releases)).await
    }
}
impl Drop for PreparedResources {
    fn drop(&mut self) {
        if !self.releases.is_empty() {
            let releases = std::mem::take(&mut self.releases);
            if let Ok(executor) = tokio::runtime::Handle::try_current() {
                executor.spawn(async move {
                    let _ = release_all(releases).await;
                });
            }
        }
    }
}
async fn release_all(mut releases: Vec<Release>) -> RuntimeResult {
    let mut errors = vec![];
    while let Some(release) = releases.pop() {
        // One resource panic must not skip the remaining releases.
        match tokio::spawn(async move { release().await }).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => errors.push(e),
            Err(e) => errors.push(RuntimeError::new(e.to_string())),
        }
    }
    super::handle::aggregate("Failed to release prepared session resources.", errors)
}

/// The prepared runtime creates the existing production host after restoration,
/// so all session ports can bind this exact checkpoint and recorder.
pub type HostBuilder =
    Box<dyn FnOnce(SessionBinding) -> Result<TurnHostFactory, RuntimeError> + Send>;
pub struct PreparedSession {
    pub resources: PreparedResources,
    pub host: HostBuilder,
    pub tail: TailConfig,
}
#[derive(Clone)]
pub struct SessionBinding {
    pub context: SessionContext,
    pub recorder: Arc<Recorder>,
    pub checkpoint: HostToolCheckpoint,
    pub projection: Arc<SessionProjection>,
}

/// Persistence is host-owned, just like TS options.createStorage. Native JSONL
/// callers use `new`; explicit restore/checkpoint/dispose hooks support external
/// providers without reporting success for an unavailable capability.
pub struct FactoryStorage {
    writer_lease: SharedWriterLease,
    pub recorder: Arc<Recorder>,
    pub projection: Arc<SessionProjection>,
    pub cwd: PathBuf,
    pub load: Arc<dyn Fn() -> BoxFuture<Result<TranscriptRead, RuntimeError>> + Send + Sync>,
    pub load_checkpoint: Arc<dyn Fn() -> BoxFuture<RuntimeResult> + Send + Sync>,
    pub close: Arc<dyn Fn() -> BoxFuture<RuntimeResult> + Send + Sync>,
}
impl FactoryStorage {
    /// Supply an unlocked store (for JSONL, `JsonlTranscriptStore::new`).
    /// The factory acquires the canonical writer lease before restore. A store
    /// already owned by `open_locked` / `PersistenceResources::create_session`
    /// would contend with that lease; #589 must transfer ownership before using
    /// that combination, rather than bypassing the factory's writer fence.
    pub fn new(store: Arc<dyn TranscriptStore>, key: TranscriptKey) -> Self {
        let projection = Arc::new(SessionProjection::default());
        let writer_lease = Arc::new(Mutex::new(None));
        let projected = Arc::new(ProjectedStore {
            _writer_lease: writer_lease.clone(),
            inner: store.clone(),
            key: key.clone(),
            projection: projection.clone(),
        });
        let recorder = Arc::new(Recorder::new(projected, key.clone()));
        let close_writer = recorder.writer().clone();
        Self {
            writer_lease,
            cwd: key.project_root.clone(),
            recorder,
            projection,
            load: Arc::new(move || {
                let store = store.clone();
                let key = key.clone();
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || {
                        store
                            .read(&key, None)
                            .map_err(|e| storage_error(e, &key.transcript_path))
                    })
                    .await
                    .map_err(|e| RuntimeError::new(e.to_string()))?
                })
            }),
            // Native checkpoint is only a cache. Replaying the authoritative
            // complete transcript is valid when it is absent or unreadable.
            load_checkpoint: Arc::new(|| Box::pin(async { Ok(()) })),
            close: Arc::new(move || {
                let writer = close_writer.clone();
                writer.begin_close();
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || writer.close())
                        .await
                        .map_err(|e| RuntimeError::new(e.to_string()))
                })
            }),
        }
    }
    fn take_writer_lease(&self) -> WriterCloseGuard {
        WriterCloseGuard {
            writer: self.recorder.writer().clone(),
            _lease: lock(&self.writer_lease).take(),
        }
    }
    async fn close_owned(&self) -> RuntimeResult {
        // Rollback has no OwnedSession guard. Keep the lease on this future's
        // stack so a hook panic or blocking-task JoinError cannot strand it in
        // an externally retained storage/recorder. Preserve final hook writes.
        let _writer_lease = self.take_writer_lease();
        let result = (self.close)().await;
        let writer = self.recorder.writer().clone();
        writer.begin_close();
        tokio::task::spawn_blocking(move || writer.close())
            .await
            .map_err(|e| RuntimeError::new(e.to_string()))?;
        result
    }
    async fn restore(&self) -> Result<TranscriptRead, RuntimeError> {
        let read = (self.load)().await?;
        read.ensure_writable()
            .map_err(|e| RuntimeError::new(e.to_string()))?;
        self.recorder
            .restore(&read.entries)
            .map_err(|e| RuntimeError::new(e.to_string()))?;
        (self.load_checkpoint)().await?;
        self.projection.restore(read.entries.clone())?;
        validate_web_history(&read.entries)?;
        Ok(read)
    }
}

pub type Prepare = Arc<
    dyn Fn(
            SessionContext,
            SessionMcpRuntimeRegistry,
        ) -> BoxFuture<Result<PreparedSession, RuntimeError>>
        + Send
        + Sync,
>;
pub type CreateStorage =
    Arc<dyn Fn(SessionContext) -> BoxFuture<Result<FactoryStorage, RuntimeError>> + Send + Sync>;
pub type Admission =
    Arc<dyn Fn(SessionContext) -> BoxFuture<Result<Option<JsValue>, RuntimeError>> + Send + Sync>;
pub struct FactoryOptions {
    /// Predecessor drain + occupied process-lock wait budget, chosen by the
    /// host's cancellation policy (e.g. POST_CANCEL_EXIT_TIMEOUT), not a total
    /// wall-clock deadline for create. Must be positive. Prepare, storage
    /// construction, path resolution and the single OS file-open/lock attempt
    /// are excluded: blocking filesystem I/O cannot be cancelled by this budget.
    /// Resolution time still consumes the remaining process-lock wait; at
    /// expiry only a nonblocking process-lock attempt is permitted.
    pub writer_busy_wait_timeout: std::time::Duration,
    pub prepare: Prepare,
    pub create_storage: CreateStorage,
    pub admission: Option<Admission>,
    pub diagnostic: Arc<dyn Fn(&Diagnostic) + Send + Sync>,
}
#[derive(Clone)]
pub struct ProjectSessionFactory {
    options: Arc<FactoryOptions>,
    mcp: SessionMcpRuntimeRegistry,
}
impl ProjectSessionFactory {
    /// # Panics
    /// Panics for a zero writer fence budget (a host configuration error).
    pub fn new(options: FactoryOptions) -> Self {
        assert!(
            !options.writer_busy_wait_timeout.is_zero(),
            "writer_busy_wait_timeout must be positive"
        );
        Self {
            options: Arc::new(options),
            mcp: SessionMcpRuntimeRegistry::new(),
        }
    }
    /// Intentionally only the per-session MCP registry; owner.shutdown owns
    /// in-flight creations and session handles (TS ProjectSessionFactory:121).
    pub async fn dispose(&self) -> RuntimeResult {
        self.mcp.dispose().await
    }

    async fn construct(
        &self,
        context: SessionContext,
        previous: Option<LiveSessionHandle>,
    ) -> Result<LiveSessionHandle, RuntimeError> {
        let prepared = (self.options.prepare)(context.clone(), self.mcp.clone()).await?;
        let captured_reload = previous
            .as_ref()
            .map(LiveSessionHandle::recreation_snapshot)
            .transpose();
        let mut reload = match captured_reload {
            Ok(value) => value,
            Err(error) => {
                let _ = prepared.resources.release().await;
                return Err(error);
            }
        };
        // A dropped timeout waiter cannot cancel the handle's owned drain task.
        let deadline = tokio::time::Instant::now() + self.options.writer_busy_wait_timeout;
        if let Some(previous) = &previous {
            let drained =
                tokio::time::timeout_at(deadline, previous.dispose("session_recreate".into()))
                    .await;
            let outcome = drained.unwrap_or_else(|_| Err(writer_busy()));
            if let Err(error) = outcome {
                let _ = prepared.resources.release().await;
                return Err(error);
            }
        }
        if let (Some(previous), Some(reload)) = (&previous, &mut reload) {
            // OwnedTurn finalizes its checkpoint before releasing handle activity.
            // Keep the earlier eligibility check, but seed the replacement only
            // after that drain; durable messages still come from storage.restore.
            match previous.drained_file_state() {
                Ok(file_state) => reload.file_state = file_state,
                Err(error) => {
                    let _ = prepared.resources.release().await;
                    return Err(error);
                }
            }
        }
        let writer_wait_remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let storage = match (self.options.create_storage)(context.clone()).await {
            Ok(storage) => storage,
            Err(error) => {
                let _ = prepared.resources.release().await;
                return Err(error);
            }
        };
        let storage = Arc::new(storage);
        // Creating storage is not waiting for a writer. Slow providers must not
        // spend the remaining drain/lease budget before lease acquisition begins.
        let deadline = tokio::time::Instant::now() + writer_wait_remaining;
        let initialized = async {
            let lease =
                acquire_writer(storage.recorder.writer().path().to_owned(), deadline).await?;
            *lock(&storage.writer_lease) = Some(lease);
            if previous.is_none()
                && let Some(admission) = &self.options.admission
            {
                let read = storage.restore().await?;
                if read.entries.is_empty()
                    && read
                        .diagnostics
                        .iter()
                        .all(|d| d.code == "transcript_missing")
                    && let Some(value) = admission(context.clone()).await?
                {
                    let mut metadata = JsObject::new();
                    metadata.insert("staffDeckAdmission".into(), value);
                    storage
                        .recorder
                        .writer()
                        .record_session_metadata(
                            &context.session_key,
                            &"host-admission".into(),
                            JsValue::Object(metadata),
                        )
                        .map_err(|e| RuntimeError::new(e.to_string()))?;
                }
            }
            let read = storage.restore().await?;
            for diagnostic in &read.diagnostics {
                (self.options.diagnostic)(diagnostic);
            }
            let checkpoint = HostToolCheckpoint::new(reload.as_ref().map(|r| &r.file_state), &[])
                .map_err(|e| RuntimeError::new(e.to_string()))?;
            let binding = SessionBinding {
                context: context.clone(),
                recorder: storage.recorder.clone(),
                checkpoint: checkpoint.clone(),
                projection: storage.projection.clone(),
            };
            let host = (prepared.host)(binding)?;
            let projection = storage.projection.clone();
            let tail = SessionTail::new(
                storage.recorder.clone(),
                context.session_key.clone(),
                prepared.tail,
            );
            let state = storage.projection.snapshot()?;
            tail.restore(state.metadata);
            let saved_files = Arc::new(Mutex::new(
                reload
                    .as_ref()
                    .map(|r| r.file_state.clone())
                    .unwrap_or_default(),
            ));
            let reload_files = saved_files.clone();
            let source = ReloadSource {
                session_id: context.session_key.clone(),
                cwd: storage.cwd.clone(),
                transcript_path: storage.recorder.writer().path().to_owned(),
                projection: storage.projection.clone(),
                file_state: Arc::new(move || Ok(lock(&reload_files).clone())),
            };
            Ok::<_, RuntimeError>((
                host.into_live_session(storage.recorder.clone(), move || {
                    lock(&projection.last_valid).messages.clone()
                })
                .with_tail(tail)
                .with_reload_source(source.clone()),
                checkpoint,
                saved_files,
                source,
            ))
        }
        .await;
        match initialized {
            Ok((adapter, checkpoint, saved_files, reload_source)) => Ok(LiveSessionHandle::new(
                Arc::new(OwnedSession {
                    adapter,
                    reload_source,
                    storage,
                    checkpoint,
                    saved_files,
                    resources: Mutex::new(Some(prepared.resources)),
                }),
                None,
            )),
            Err(error) => {
                // The outer TS create catch swallows prepared-release errors.
                // recreate reverses the order relative to resume rollback.
                let rollback = if previous.is_some() {
                    let _ = prepared.resources.release().await;
                    storage.close_owned().await
                } else {
                    let rollback = storage.close_owned().await;
                    let _ = prepared.resources.release().await;
                    rollback
                };
                match rollback {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(RuntimeError {
                        message: format!(
                            "Failed to {} and roll back agent session {}.",
                            if previous.is_some() {
                                "recreate"
                            } else {
                                "resume"
                            },
                            String::from_utf16_lossy(context.session_key.units())
                        ),
                        causes: vec![error, rollback],
                        js_error: None,
                    }),
                }
            }
        }
    }
}
impl SessionFactory for ProjectSessionFactory {
    fn can_recreate(&self) -> bool {
        true
    }
    fn create(
        &self,
        context: SessionContext,
        previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let factory = self.clone();
        Box::pin(async move {
            // Like a JS Promise, dropping its waiter cannot abandon acquisition.
            // If the receiver is gone, dispose the completed candidate instead.
            let (send, receive) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let result = factory
                    .construct(context, previous)
                    .await
                    .map(PendingSession::new);
                let _ = send.send(result);
            });
            receive
                .await
                .map_err(|e| RuntimeError::new(e.to_string()))?
                .map(|mut candidate| candidate.handle.take().expect("factory candidate handle"))
        })
    }
}
/// A successful send does not prove that a cancelled waiter consumed it.
struct PendingSession {
    handle: Option<LiveSessionHandle>,
    executor: tokio::runtime::Handle,
}
impl PendingSession {
    fn new(handle: LiveSessionHandle) -> Self {
        Self {
            handle: Some(handle),
            executor: tokio::runtime::Handle::current(),
        }
    }
}
impl Drop for PendingSession {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.executor.spawn(async move {
                let _ = handle.dispose("session_create_cancelled".into()).await;
            });
        }
    }
}
struct OwnedSession {
    adapter: LiveSessionAdapter,
    reload_source: ReloadSource,
    checkpoint: HostToolCheckpoint,
    saved_files: Arc<Mutex<SeedState>>,
    storage: Arc<FactoryStorage>,
    resources: Mutex<Option<PreparedResources>>,
}
impl LiveSession for OwnedSession {
    #[cfg(test)]
    fn reload_signal_for_test(&self) -> Option<crate::runtime::hooks::Cancellation> {
        self.adapter.reload_signal_for_test()
    }
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        let turn = self.adapter.submit(input, options)?;
        Ok(Box::new(OwnedTurn {
            turn,
            checkpoint: self.checkpoint.clone(),
            saved: self.saved_files.clone(),
            started: self.adapter.reload_execution_started(),
            finished: false,
        }))
    }
    fn snapshot(&self) -> JsValue {
        self.adapter.snapshot()
    }
    fn reload_snapshot(&self) -> Result<ReloadSnapshot, RuntimeError> {
        self.adapter.reload_snapshot()
    }
    fn draining_reload_snapshot(&self) -> Result<ReloadSnapshot, RuntimeError> {
        self.reload_source
            .snapshot_consistent(&self.storage.recorder)
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        self.adapter.abort(reason)
    }
    fn disposal_guard(&self) -> Option<Box<dyn Send>> {
        Some(Box::new(self.storage.take_writer_lease()))
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        // Handle disposal has already drained active turns. Take ownership
        // before any adapter/resource hook can panic; retain through cleanup.
        let writer_lease = self.storage.take_writer_lease();
        let resources = lock(&self.resources).take();
        Box::pin(async move {
            let _writer_lease = writer_lease;
            let mut errors = vec![];
            if let Some(resources) = resources
                && let Err(e) = resources.release().await
            {
                errors.push(e);
            }
            if let Err(e) = self.adapter.dispose().await {
                errors.push(e);
            }
            if let Err(e) = self.storage.close_owned().await {
                errors.push(e);
            }
            super::handle::aggregate("Failed to dispose factory session.", errors)
        })
    }
}

struct OwnedTurn {
    turn: Box<dyn LiveTurn>,
    checkpoint: HostToolCheckpoint,
    saved: Arc<Mutex<SeedState>>,
    started: Arc<AtomicBool>,
    finished: bool,
}
impl LiveTurn for OwnedTurn {
    fn next(&mut self) -> super::SessionNext<'_> {
        Box::pin(async move {
            let result = self.turn.next().await;
            let terminal = matches!(&result, Ok(Some(event)) if is(field(event,"type"),"turn_completed"))
                || !matches!(&result, Ok(Some(_)));
            if !self.finished
                && (matches!(&result, Ok(Some(event)) if is(field(event,"type"),"turn_completed"))
                    || (self.started.load(Ordering::SeqCst) && !matches!(&result, Ok(Some(_)))))
            {
                *lock(&self.saved) = self
                    .checkpoint
                    .snapshot()
                    .map_err(|e| RuntimeError::new(e.to_string()))?;
            }
            self.finished |= terminal;
            result
        })
    }
}
impl Drop for OwnedTurn {
    fn drop(&mut self) {
        if !self.finished
            && self.started.load(Ordering::SeqCst)
            && let Ok(seed) = self.checkpoint.snapshot()
        {
            *lock(&self.saved) = seed;
        }
    }
}

fn storage_error(error: crate::storage::LedgerError, path: &std::path::Path) -> RuntimeError {
    if let crate::storage::LedgerError::Io(io) = &error
        && let Some((code, message)) = crate::node_io::read_file(io, &path.to_string_lossy())
    {
        return RuntimeError::new(message.clone()).with_js_error(
            crate::runtime::project::RuntimeJsError {
                name: "Error".into(),
                code: Some(code.into()),
                message: message.into(),
                requires_host: false,
            },
        );
    }
    RuntimeError::new(error.to_string())
}
fn clone_message(message: &JsValue) -> Result<JsValue, RuntimeError> {
    if matches!(message, JsValue::Null | JsValue::Undefined) {
        return Err(RuntimeError::new(
            "Cannot read properties of null or undefined (reading 'content')",
        ));
    }
    let mut object = message.as_object().cloned().unwrap_or_default();
    let blocks = match field(message, "content") {
        JsValue::Array(blocks) => blocks.clone(),
        _ => vec![],
    };
    for block in &blocks {
        if matches!(block, JsValue::Null | JsValue::Undefined) {
            return Err(RuntimeError::new(
                "Cannot read properties of null or undefined (reading 'type')",
            ));
        }
        if is(field(block, "type"), "tool_result") {
            array(field(block, "content"))?;
        }
    }
    object.insert("content".into(), JsValue::Array(blocks));
    Ok(JsValue::Object(object))
}
fn clone_messages(messages: &JsValue) -> Result<Vec<JsValue>, RuntimeError> {
    array(messages)?.iter().map(clone_message).collect()
}
fn valid_snapshot_message(message: &JsValue) -> bool {
    message.as_object().is_some()
        && (is(field(message, "role"), "user") || is(field(message, "role"), "assistant"))
        && (field(message, "metadata").is_undefined()
            || field(message, "metadata").as_object().is_some())
        && array(field(message, "content"))
            .is_ok_and(|blocks| blocks.iter().all(valid_snapshot_block))
}
fn valid_snapshot_block(block: &JsValue) -> bool {
    if block.as_object().is_none() {
        return false;
    }
    let text = |key| field(block, key).as_string().is_some();
    let kind = field(block, "type");
    if is(kind, "text") || is(kind, "thinking") {
        return text("text");
    }
    if is(kind, "image") || is(kind, "audio") {
        return (is(field(block, "source"), "base64") || is(field(block, "source"), "url"))
            && text("data")
            && text("mimeType");
    }
    if is(kind, "pdf") {
        return is(field(block, "source"), "base64")
            && text("data")
            && is(field(block, "mimeType"), "application/pdf")
            && field(block, "bytes").as_number().is_some();
    }
    if is(kind, "tool_call") {
        return text("id") && text("name");
    }
    if is(kind, "tool_result") {
        return text("toolCallId")
            && array(field(block, "content")).is_ok_and(|blocks| {
                blocks.iter().all(|b| {
                    ["text", "image", "pdf"]
                        .iter()
                        .any(|k| is(field(b, "type"), k))
                        && valid_snapshot_block(b)
                })
            });
    }
    if is(kind, "tool_result_reference") || is(kind, "media_reference") {
        return text("path")
            && field(block, "originalBytes").as_number().is_some()
            && text("preview")
            && field(block, "hasMore").as_bool().is_some()
            && if is(kind, "tool_result_reference") {
                text("toolCallId")
            } else {
                text("mimeType")
                    && ["image", "pdf", "audio"]
                        .iter()
                        .any(|k| is(field(block, "mediaType"), k))
            };
    }
    false
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    {
        static UNDEFINED: JsValue = JsValue::Undefined;
        value.get(key).unwrap_or(&UNDEFINED)
    }
}
fn type_error(message: impl Into<String>) -> RuntimeError {
    let message = message.into();
    RuntimeError::new(message.clone()).with_js_error(crate::runtime::project::RuntimeJsError {
        name: "TypeError".into(),
        code: None,
        message: message.into(),
        requires_host: false,
    })
}
fn property<'a>(value: &'a JsValue, key: &str) -> Result<&'a JsValue, RuntimeError> {
    if matches!(value, JsValue::Null | JsValue::Undefined) {
        return Err(type_error(format!(
            "Cannot read properties of {} (reading '{key}')",
            if matches!(value, JsValue::Null) {
                "null"
            } else {
                "undefined"
            }
        )));
    }
    Ok(field(value, key))
}
fn add_usage(first: &JsValue, second: &JsValue) -> Result<JsValue, RuntimeError> {
    let primitive = |value: &JsValue| -> Result<JsValue, RuntimeError> {
        match value {
            JsValue::Null | JsValue::Undefined => Ok(JsValue::Number(0.0)),
            JsValue::Object(_) | JsValue::Array(_) => {
                crate::js_value::to_string_units_checked(value)
                    .map(|s| JsValue::String(JsString::from_utf16(s)))
                    .map_err(|e| type_error(e.message()))
            }
            _ => Ok(value.clone()),
        }
    };
    let (first, second) = (primitive(first)?, primitive(second)?);
    if matches!(first, JsValue::String(_)) || matches!(second, JsValue::String(_)) {
        let mut text = crate::js_value::to_string_units_checked(&first)
            .map_err(|e| type_error(e.message()))?;
        text.extend(
            crate::js_value::to_string_units_checked(&second)
                .map_err(|e| type_error(e.message()))?,
        );
        Ok(JsValue::String(JsString::from_utf16(text)))
    } else {
        let first = crate::js_value::to_number(&first)
            .map_err(|_| type_error("Maximum call stack size exceeded"))?;
        let second = crate::js_value::to_number(&second)
            .map_err(|_| type_error("Maximum call stack size exceeded"))?;
        Ok(JsValue::Number(first + second))
    }
}
fn spread(value: &JsValue) -> JsObject {
    if let JsValue::Object(object) = value {
        return object.clone();
    }
    let items = match value {
        JsValue::Array(items) => items.clone(),
        JsValue::String(text) => text
            .units()
            .iter()
            .map(|u| JsValue::String(JsString::from_utf16(vec![*u])))
            .collect(),
        _ => Vec::new(),
    };
    let mut object = JsObject::new();
    for (i, value) in items.into_iter().enumerate() {
        object.insert(i.to_string().into(), value);
    }
    object
}
/// The default storage registry also hydrates WebHistory, even for incomplete
/// turns. Its `.some` walk short-circuits on the first tool result.
fn validate_web_history(entries: &[JsValue]) -> RuntimeResult {
    for entry in entries {
        if ![
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .iter()
        .any(|k| is(field(entry, "type"), k))
        {
            continue;
        }
        let content = property(field(entry, "message"), "content")?;
        property(content, "some")?;
        let JsValue::Array(blocks) = content else {
            return Err(type_error("message.content.some is not a function"));
        };
        for block in blocks {
            let kind = property(block, "type")?;
            if is(kind, "tool_result") || is(kind, "tool_result_reference") {
                break;
            }
        }
    }
    Ok(())
}
fn is(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
}
fn array(value: &JsValue) -> Result<&[JsValue], RuntimeError> {
    match value {
        JsValue::Array(items) => Ok(items),
        _ => Err(RuntimeError::new("Session projection requires an array.")),
    }
}
fn merge_metadata(target: &mut JsObject, metadata: &JsObject) {
    for (key, value) in metadata.entries() {
        if (key.equals_str("title") || key.equals_str("linkedPullRequest"))
            && matches!(value, JsValue::Null | JsValue::Undefined)
        {
            continue;
        }
        target.insert(key.clone(), value.clone());
    }
}
fn real_user(message: &JsValue) -> bool {
    is(field(message, "role"), "user") && field(field(message,"metadata"),"synthetic") != &JsValue::Bool(true)
        && array(field(message,"content")).is_ok_and(|blocks| blocks.iter().any(|block| {
            let kind = field(block,"type");
            if is(kind,"tool_result") || is(kind,"tool_result_reference") || (is(kind,"media_reference") && field(block,"toolCallId").as_string().is_some()) { return false; }
            if !is(kind,"text") { return true; }
            let text = field(block,"text").as_string().map(|s| String::from_utf16_lossy(s.units())).unwrap_or_default();
            let text = crate::metadata::js_trim(&text);
            !text.is_empty() && text != "[system: the conversation above has been compacted. please continue with the current task.]" && !["<compact-boundary","<snip-boundary","<memory-context>","<internal-compaction-control","<hook_context"].iter().any(|p| text.starts_with(p))
        }))
}
fn project(entries: &[JsValue]) -> Result<DurableSessionState, RuntimeError> {
    let completed: HashSet<_> = entries
        .iter()
        .filter(|e| is(field(e, "type"), "turn_result"))
        .filter_map(|e| field(e, "turnId").as_string())
        .collect();
    let mut replacement = None;
    for (index, entry) in entries.iter().enumerate() {
        let boundary = field(entry, "boundary");
        if is(field(entry, "type"), "control_boundary")
            && is(field(boundary, "kind"), "compact")
            && is(field(boundary, "subtype"), "compact_boundary")
        {
            let snapshot = field(boundary, "snapshot");
            let messages = field(snapshot, "messages");
            let valid = field(snapshot, "version").as_number() == Some(1.0)
                && array(messages)
                    .is_ok_and(|m| !m.is_empty() && m.iter().all(valid_snapshot_message));
            let messages = if valid {
                messages
            } else {
                field(boundary, "replacementMessages")
            };
            if !matches!(
                messages,
                JsValue::Undefined | JsValue::Null | JsValue::Bool(false)
            ) {
                replacement = Some((index, clone_messages(messages)?, valid));
            }
        }
    }
    let mut result = DurableSessionState::default();
    for (index, entry) in entries.iter().enumerate() {
        let kind = field(entry, "type");
        let before = replacement.as_ref().is_some_and(|(b, _, _)| index < *b);
        let done = field(entry, "turnId")
            .as_string()
            .is_some_and(|id| completed.contains(id));
        if is(kind, "accepted_input") && !before {
            result
                .messages
                .extend(clone_messages(field(entry, "messages"))?);
        }
        if [
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .iter()
        .any(|s| is(kind, s))
            && !before
            && done
        {
            let message = field(entry, "message");
            if field(field(message, "metadata"), "compactReplacement") != &JsValue::Bool(true) {
                result.messages.push(clone_message(message)?);
            }
        }
        if is(kind, "context_snapshot")
            && !before
            && done
            && let JsValue::Array(messages) = field(entry, "runtimeContextMessages")
        {
            let at = result
                .messages
                .iter()
                .rposition(real_user)
                .unwrap_or(result.messages.len());
            result
                .messages
                .splice(at..at, clone_messages(&JsValue::Array(messages.clone()))?);
        }
        if let Some((b, messages, valid)) = &replacement
            && index == *b
            && (*valid || done)
        {
            result.messages.extend(messages.clone());
        }
        if is(kind, "turn_result") {
            let turn = field(entry, "result");
            let usage = property(turn, "usage")?;
            for name in [
                "inputTokens",
                "outputTokens",
                "cacheReadTokens",
                "cacheWriteTokens",
                "totalTokens",
            ] {
                let value = property(usage, name)?;
                let prior = result
                    .usage
                    .get(name)
                    .cloned()
                    .unwrap_or(JsValue::Undefined);
                if !prior.is_undefined() || !value.is_undefined() {
                    let sum = add_usage(&prior, value)?;
                    result.usage.insert(name.into(), sum);
                }
            }
            result
                .permission_denials
                .extend_from_slice(array(field(turn, "permissionDenials"))?);
        }
        if is(kind, "session_metadata") {
            let metadata = field(entry, "metadata");
            property(metadata, "title")?;
            property(metadata, "linkedPullRequest")?;
            merge_metadata(&mut result.metadata, &spread(metadata));
        }
        if is(kind, "accepted_input") {
            let selection = field(field(entry, "metadata"), "modelSelection");
            if is(field(selection, "mode"), "auto")
                || (is(field(selection, "mode"), "model")
                    && field(selection, "provider").as_string().is_some()
                    && field(selection, "model").as_string().is_some())
            {
                result
                    .metadata
                    .insert("modelSelection".into(), selection.clone());
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
#[path = "factory_tests.rs"]
mod tests;
