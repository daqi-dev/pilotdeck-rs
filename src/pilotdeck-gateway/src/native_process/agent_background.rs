//! Session-owned fork generation. Detached task ownership extends this same owner.
use super::*;
use crate::runtime::{
    project::{RuntimeLease, RuntimeResult},
    refresh::BoxFuture,
};
use std::sync::atomic::AtomicBool;

const RAW_EVENT_COUNT: usize = 4096;
const RAW_EVENT_BYTES: usize = 16 * 1024 * 1024;
struct BufferedRawEvent {
    event: JsObject,
    _foreground: Option<tokio::sync::OwnedSemaphorePermit>,
}
#[derive(Default)]
struct RawEvents {
    events: Vec<BufferedRawEvent>,
    bytes: usize,
}

struct OwnedForkJob {
    cancel: Cancellation,
    done: watch::Receiver<Option<RuntimeResult>>,
}
pub(crate) struct NativeForkOwner {
    generation: Mutex<Option<Arc<RuntimeLease>>>,
    raw: Mutex<RawEvents>,
    closed: AtomicBool,
    releasing: Mutex<bool>,
    completion: watch::Sender<Option<RuntimeResult>>,
    executor: tokio::runtime::Handle,
    tasks: Mutex<
        Vec<(
            crate::runtime::background::BackgroundSubagentRuntime,
            JsString,
        )>,
    >,
    drain_started: Mutex<bool>,
    drain_done: watch::Sender<Option<RuntimeResult>>,
    cleanup_errors: Mutex<Vec<String>>,
    owned_jobs: Mutex<Vec<OwnedForkJob>>,
    hook_scopes: Mutex<Vec<super::lifecycle::DetachedHooks>>,
}
impl NativeForkOwner {
    pub(in crate::native_process) fn new(lease: RuntimeLease) -> Arc<Self> {
        Arc::new(Self {
            generation: Mutex::new(Some(Arc::new(lease))),
            raw: Mutex::default(),
            closed: AtomicBool::new(false),
            releasing: Mutex::new(false),
            completion: watch::channel(None).0,
            executor: tokio::runtime::Handle::current(),
            tasks: Mutex::default(),
            drain_started: Mutex::new(false),
            drain_done: watch::channel(None).0,
            cleanup_errors: Mutex::default(),
            owned_jobs: Mutex::default(),
            hook_scopes: Mutex::default(),
        })
    }
    pub(in crate::native_process) fn generation(&self) -> Option<Arc<RuntimeLease>> {
        let generation = lock(&self.generation);
        if self.closed.load(Ordering::SeqCst) {
            None
        } else {
            generation.clone()
        }
    }
    pub(super) fn push_raw(
        &self,
        event: JsObject,
        foreground: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<(), ToolError> {
        let mut raw = lock(&self.raw);
        if !self.is_open() {
            return Err(ToolError::aborted());
        }
        if raw.events.len() >= RAW_EVENT_COUNT {
            return Err(error(
                "Native session raw event buffer count budget is full.",
            ));
        }
        let mut value = JsValue::Object(event);
        let bytes = pilotdeck_protocol::stringify_json(&value)
            .map_err(|failure| error(failure.to_string()))?
            .map_or(0, |value| value.len());
        let total = raw
            .bytes
            .checked_add(bytes)
            .filter(|bytes| *bytes <= RAW_EVENT_BYTES)
            .ok_or_else(|| error("Native session raw event buffer byte budget is full."))?;
        if !self.is_open() {
            return Err(ToolError::aborted());
        }
        let JsValue::Object(event) = &mut value else {
            unreachable!()
        };
        raw.events.push(BufferedRawEvent {
            event: std::mem::take(event),
            _foreground: foreground,
        });
        raw.bytes = total;
        Ok(())
    }
    pub(super) fn drain_raw(&self) -> Vec<JsObject> {
        let events = {
            let mut raw = lock(&self.raw);
            raw.bytes = 0;
            std::mem::take(&mut raw.events)
        };
        events.into_iter().map(|entry| entry.event).collect()
    }
    pub(in crate::native_process) fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }
    fn start(
        &self,
        runtime: crate::runtime::background::BackgroundSubagentRuntime,
        input: crate::runtime::background::StartBackgroundSubagentInput,
    ) -> Result<crate::runtime::background::BackgroundSubagentTask, ToolError> {
        let mut tasks = lock(&self.tasks);
        if !self.is_open() {
            return Err(ToolError::aborted());
        }
        if tasks.len() >= 256 {
            return Err(error(
                "Native session background task record budget is full.",
            ));
        }
        let task = runtime.start(input);
        tasks.push((runtime, task.task_id.clone()));
        Ok(task)
    }
    pub(super) fn retain_hooks(
        &self,
        hooks: super::lifecycle::DetachedHooks,
    ) -> Result<(), ToolError> {
        let mut scopes = lock(&self.hook_scopes);
        if !self.is_open() {
            return Err(ToolError::aborted());
        }
        if scopes.len() >= 256 {
            return Err(error("Native detached hook scope budget is full."));
        }
        scopes.push(hooks);
        Ok(())
    }
    pub(super) fn spawn_owned(
        self: &Arc<Self>,
        run: impl FnOnce(Cancellation) -> BoxFuture<RuntimeResult> + Send + 'static,
    ) -> Result<(), ToolError> {
        let mut jobs = lock(&self.owned_jobs);
        jobs.retain(|job| !matches!(&*job.done.borrow(), Some(Ok(()))));
        if !self.is_open() {
            return Err(ToolError::aborted());
        }
        if jobs.len() >= 256 {
            return Err(error("Native session owned job budget is full."));
        }
        let cancel = Cancellation::new();
        let (done, receiver) = watch::channel(None);
        jobs.push(OwnedForkJob {
            cancel: cancel.clone(),
            done: receiver,
        });
        crate::runtime::cleanup_task::spawn_with_error(
            &self.executor,
            done,
            RuntimeError::new("Native session owned job was dropped or panicked."),
            async move { run(cancel).await },
        );
        Ok(())
    }
    pub(in crate::native_process) fn close_admission(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for hooks in lock(&self.hook_scopes).iter() {
            hooks.cancel();
        }
        {
            let mut raw = lock(&self.raw);
            raw.events.clear();
            raw.bytes = 0;
        }
        for job in lock(&self.owned_jobs).iter() {
            job.cancel.cancel();
        }
        let tasks = lock(&self.tasks).clone();
        // Cancel all before waiting; one noncooperative child cannot prevent
        // cancellation from reaching its independent siblings.
        for (runtime, id) in tasks {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.stop(&id))).is_err()
            {
                lock(&self.cleanup_errors).push("Background cancellation panicked.".into());
            }
        }
    }
    pub(in crate::native_process) fn drain(self: Arc<Self>) -> BoxFuture<RuntimeResult> {
        self.close_admission();
        let mut done = self.drain_done.subscribe();
        let first = {
            let mut started = lock(&self.drain_started);
            let first = !*started;
            *started = true;
            first
        };
        if first {
            let owner = self.clone();
            crate::runtime::cleanup_task::spawn_with_error(
                &self.executor,
                self.drain_done.clone(),
                RuntimeError::new("Native background drain was dropped or panicked."),
                async move {
                    let tasks = lock(&owner.tasks).clone();
                    for (runtime, id) in tasks {
                        runtime.stop_and_wait(&id).await;
                    }
                    let jobs = lock(&owner.owned_jobs)
                        .iter()
                        .map(|job| job.done.clone())
                        .collect::<Vec<_>>();
                    for mut done in jobs {
                        loop {
                            let result = done.borrow_and_update().clone();
                            if let Some(result) = result {
                                if let Err(error) = result {
                                    lock(&owner.cleanup_errors).push(error.to_string());
                                }
                                break;
                            }
                            if done.changed().await.is_err() {
                                lock(&owner.cleanup_errors)
                                    .push("Native session owned job completion lost.".into());
                                break;
                            }
                        }
                    }
                    let hooks = std::mem::take(&mut *lock(&owner.hook_scopes));
                    for scope in hooks {
                        if let Err(error) = scope.close().await {
                            lock(&owner.cleanup_errors).push(error.to_string());
                        }
                    }
                    let failures = lock(&owner.cleanup_errors).clone();
                    if failures.is_empty() {
                        Ok(())
                    } else {
                        Err(RuntimeError::new(failures.join("; ")))
                    }
                },
            );
        }
        Box::pin(async move {
            loop {
                if let Some(result) = done.borrow_and_update().clone() {
                    return result;
                }
                done.changed()
                    .await
                    .map_err(|_| RuntimeError::new("Native background drain completion lost."))?;
            }
        })
    }
    pub(in crate::native_process) fn release_generation(
        self: Arc<Self>,
    ) -> BoxFuture<RuntimeResult> {
        self.close_admission();
        let mut completion = self.completion.subscribe();
        let first = {
            let mut releasing = lock(&self.releasing);
            let first = !*releasing;
            *releasing = true;
            first
        };
        if first {
            let owner = self.clone();
            crate::runtime::cleanup_task::spawn_with_error(
                &self.executor,
                self.completion.clone(),
                RuntimeError::new("Native fork generation release was dropped or panicked."),
                async move {
                    let drained = owner.clone().drain().await;
                    let generation = lock(&owner.generation).take();
                    let released = match generation {
                        Some(generation) => match Arc::try_unwrap(generation) {
                            Ok(mut lease) => lease.release().await,
                            Err(mut generation) => loop {
                                // Do not detach a still-active child or pretend its
                                // execution world is released. The enclosing drain
                                // retains this future and reports its own deadline.
                                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                                match Arc::try_unwrap(generation) {
                                    Ok(mut lease) => break lease.release().await,
                                    Err(held) => generation = held,
                                }
                            },
                        },
                        None => Ok(()),
                    };
                    let errors = drained.err().into_iter().chain(released.err()).collect();
                    RuntimeError::aggregate(
                        "Failed to drain native child work and release its generation.",
                        errors,
                    )
                },
            );
        }
        Box::pin(async move {
            loop {
                if let Some(result) = completion.borrow_and_update().clone() {
                    return result;
                }
                completion
                    .changed()
                    .await
                    .map_err(|_| RuntimeError::new("Native fork generation completion lost."))?;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle, RuntimeState};
    #[tokio::test]
    async fn fork_stays_in_retired_generation_and_release_waits_for_live_borrow() {
        let registry = ProjectRuntimeRegistry::new(
            "/fixture-native-generation",
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        );
        let lease = registry.acquire(None).unwrap();
        let old = lease.runtime().clone();
        let owner = NativeForkOwner::new(lease);
        let held = owner.generation().unwrap();
        registry.reload().await.unwrap();
        let current = registry.acquire(None).unwrap();
        assert!(!Arc::ptr_eq(&old, current.runtime()));
        assert_eq!(old.state(), RuntimeState::Retired);
        assert!(old.acquire().is_err());
        let mut derivative = held.fork().unwrap();
        assert!(Arc::ptr_eq(derivative.runtime(), &old));
        assert_eq!(old.lease_count(), 2);
        derivative.release().await.unwrap();
        assert_eq!(old.lease_count(), 1);
        let mut closing = owner.clone().release_generation();
        assert!(!owner.is_open());
        assert!(owner.generation().is_none());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut closing)
                .await
                .is_err()
        );
        drop(held);
        tokio::time::timeout(std::time::Duration::from_secs(2), closing)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.state(), RuntimeState::Disposed);
        drop(current);
        registry.dispose().await.unwrap();
    }
}

struct BackgroundRecordReservation {
    registry: Arc<NativeForkRegistry>,
    committed: bool,
}
impl BackgroundRecordReservation {
    #[allow(deprecated)] // fetch_update supports the workspace MSRV (1.93).
    fn reserve(registry: Arc<NativeForkRegistry>) -> Result<Self, ToolError> {
        registry
            .background_records
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                (count < 4096).then_some(count + 1)
            })
            .map_err(|_| error("Native background task record budget is full."))?;
        Ok(Self {
            registry,
            committed: false,
        })
    }
    fn commit(mut self) {
        self.committed = true;
    }
}
impl Drop for BackgroundRecordReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.registry
                .background_records
                .fetch_sub(1, Ordering::SeqCst);
        }
    }
}

struct BackgroundRunGuard {
    owner: Arc<NativeForkOwner>,
    finished: bool,
}
impl Drop for BackgroundRunGuard {
    fn drop(&mut self) {
        if !self.finished {
            lock(&self.owner.cleanup_errors)
                .push("Native background fork stopped before joining child cleanup.".into());
        }
    }
}
pub(super) struct DetachedFork {
    pub factory: Arc<ProjectSessionFactory>,
    pub permit: Option<tokio::sync::OwnedSemaphorePermit>,
    pub definition: String,
    pub spec: JsObject,
    pub description: JsString,
    pub directive: JsString,
    pub parent_mode: JsString,
    pub parent_ask: bool,
    pub depth: f64,
    pub max: f64,
    pub context: ToolRunContext,
    pub observer: Option<super::observer::ObserverInput>,
}
impl ForkApi {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn launch_background(
        self: &Arc<Self>,
        factory: Arc<ProjectSessionFactory>,
        permit: tokio::sync::OwnedSemaphorePermit,
        definition: String,
        spec: JsObject,
        description: JsString,
        directive: JsString,
        parent_mode: JsString,
        parent_ask: bool,
        depth: f64,
        max: f64,
        context: ToolRunContext,
    ) -> Result<ToolOutput, ToolError> {
        self.launch_detached(DetachedFork {
            factory,
            permit: Some(permit),
            definition,
            spec,
            description,
            directive,
            parent_mode,
            parent_ask,
            depth,
            max,
            context,
            observer: None,
        })
    }
    pub(super) fn launch_detached(
        self: &Arc<Self>,
        input: DetachedFork,
    ) -> Result<ToolOutput, ToolError> {
        let DetachedFork {
            factory,
            permit,
            definition,
            spec,
            description,
            directive,
            parent_mode,
            parent_ask,
            depth,
            max,
            context,
            observer,
        } = input;
        let is_observer = observer.is_some();
        use crate::runtime::{
            background::{BackgroundExecutionWorld, StartBackgroundSubagentInput},
            project::{ProjectResources, Resource},
            tools::ToolCancellation,
        };
        let owner = self
            .turn
            .owner
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(|| error("Native background session owner is unavailable."))?;
        let generation = self
            .turn
            .generation
            .get()
            .cloned()
            .ok_or_else(|| error("Native background creation turn is not bound."))?;
        let resource = generation
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| error("Native background project resources are missing."))?
            .context()
            .resource(Resource::ExecutionWorld)
            .ok_or_else(|| error("Native background execution world is missing."))?;
        let runtime = resource
            .as_any()
            .downcast_ref::<BackgroundExecutionWorld>()
            .ok_or_else(|| error("Native background execution world has the wrong type."))?
            .resources
            .subagents
            .clone();
        let id: JsString = observer
            .as_ref()
            .map(|input| input.observer_subagent_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into());
        let output_id = id.clone();
        let output_definition = definition.clone();
        let output_description = description.clone();
        // Preserve the creating turn's policy. A stale/missing publisher
        // cannot grant: the existing permission emitter rejects delivery.
        let mut authority = self.turn.authority.clone();
        if let Some(authority) = &mut authority {
            authority.emit = self
                .turn
                .session_emit
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(|_| false));
        }
        // Capture immutable file-state seeds before returning task admission.
        let read = Arc::new(Mutex::new(lock(&self.read).clone()));
        let write = Arc::new(Mutex::new(lock(&self.write).clone()));
        let mut detached_config = self.turn.config.clone();
        if is_observer {
            detached_config.insert("permissionMode".into(), "plan".into());
            detached_config.insert("runMode".into(), "ask".into());
            detached_config.insert("nativeMemoryDisabled".into(), JsValue::Bool(true));
            detached_config.insert("nativeObserver".into(), JsValue::Bool(true));
            if let Some(authority) = &mut authority {
                authority.permission_mode = Some("plan".into());
            }
        }
        let mut detached = ForkTurn::new(
            self.turn.registry.clone(),
            self.turn.parent.clone(),
            detached_config,
            self.turn.recorder.clone(),
            self.turn.rules.clone(),
            authority,
            None,
            self.turn.tools.clone(),
            self.turn.skills.clone(),
            Some(&owner),
            #[cfg(unix)]
            self.turn.sandbox.clone(),
        );
        if let Some(emit) = self.turn.session_emit.get() {
            detached.bind_session_emitter(emit.clone());
        }
        // SubAgentSession inherits the original session lifecycle registration.
        // A descendant's own session has no external Gateway admission entry.
        Arc::get_mut(&mut detached)
            .expect("new detached fork is unique")
            .inherited_sdk_async = self.turn.parent_async_registration();
        Arc::get_mut(&mut detached)
            .expect("new detached turn has one owner")
            .detached = true;
        if let Some(source) = self.turn.detached_hook_source.get() {
            let hooks = source.create(&generation)?;
            owner.retain_hooks(hooks.clone())?;
            detached
                .lifecycle
                .set(Arc::new(hooks.clone()))
                .map_err(|_| error("Native detached lifecycle is already bound."))?;
            detached
                .owned_hooks
                .set(hooks)
                .map_err(|_| error("Native detached lifecycle owner is already bound."))?;
        }
        detached
            .bind_generation(generation)
            .map_err(|failure| error(failure.to_string()))?;
        let api = Arc::new(ForkApi {
            turn: detached.clone(),
            read: read.clone(),
            write: write.clone(),
        });
        let timeout = context
            .fields
            .get("subagentTimeoutMs")
            .and_then(JsValue::as_number)
            .unwrap_or(3_600_000.0);
        let deadline = tokio::time::Instant::now()
            + crate::runtime::tools::builtin::bash::runner::timer_delay(timeout);
        let mut fields = context.fields.clone();
        if is_observer {
            fields.insert("currentPermissionDecision".into(), JsValue::Undefined);
            fields.insert("currentToolCallId".into(), JsValue::Undefined);
            fields.insert("permissionMode".into(), "plan".into());
            fields.insert("runMode".into(), "ask".into());
        }
        let observer_owner = owner.clone();
        let record = BackgroundRecordReservation::reserve(self.turn.registry.clone())?;
        let slots = self.turn.registry.slots.clone();
        let fork: super::observer::ObserverFork =
            Box::new(move |request, signal: ToolCancellation| {
                Box::pin(async move {
                    let mut guard = BackgroundRunGuard {
                        owner: observer_owner.clone(),
                        finished: false,
                    };
                    let deadline = if request.native_observer {
                        tokio::time::Instant::now()
                            + crate::runtime::tools::builtin::bash::runner::timer_delay(
                                request.timeout_ms.unwrap_or(timeout),
                            )
                    } else {
                        deadline
                    };
                    let mut held_permit = None;
                    let result: Result<ToolOutput, ToolError> = async {
                        // An observer is admitted after its observed child settles,
                        // while the outer agent tool may still hold its slot. Wait
                        // inside the existing cancellable task, never fail merely
                        // because maxConcurrent=1 has not released that slot yet.
                        held_permit = Some(match permit {
                            Some(permit) => permit,
                            None => observer_slot(slots, &signal, deadline).await?,
                        });
                        let context = detached_context(fields, read, write, signal.clone());
                        let mut spec = request.definition;
                        if request.suppress_auto_observer {
                            spec.insert("observer".into(), JsValue::Undefined);
                            spec.insert("observerMessage".into(), JsValue::Undefined);
                        }
                        if request.native_observer {
                            spec.insert("nativeObserver".into(), JsValue::Bool(true));
                        }
                        let cancel = Cancellation::new();
                        let running = api.run(
                            factory,
                            String::from_utf16_lossy(request.definition_id.units()),
                            spec,
                            description,
                            request.directive,
                            if request.native_observer {
                                "plan".into()
                            } else {
                                parent_mode
                            },
                            parent_ask || request.native_observer,
                            depth,
                            max,
                            context,
                            cancel.clone(),
                            deadline,
                            Some(request.subagent_id),
                        );
                        tokio::pin!(running);
                        // Child timeout is classified by api.run's own deadline.
                        // Only real task cancellation revokes lifecycle hooks;
                        // timeout must still dispatch SubagentStop(false).
                        tokio::select! {
                            biased;
                            _=signal.cancelled()=>{
                                if let Some(hooks)=detached.owned_hooks.get(){hooks.cancel();}
                                cancel.cancel();
                                running.await
                            },

                            result=&mut running=>result,
                        }
                    }
                    .await;
                    if let Err(error) = detached.clone().close().await {
                        lock(&observer_owner.cleanup_errors).push(error.to_string());
                    }
                    lock(&observer_owner.cleanup_errors)
                        .extend(lock(&detached.cleanup_errors).iter().cloned());
                    guard.finished = true;
                    drop(held_permit);
                    result
                        .map_err(|failure| failure.message)
                        .and_then(|output| {
                            if !is_observer {
                                return Ok(JsString::default());
                            }
                            output
                                .data
                                .as_ref()
                                .and_then(|data| data.get("text"))
                                .and_then(JsValue::as_string)
                                .cloned()
                                .ok_or_else(|| {
                                    JsString::from(
                                        "Native observer fork returned no markdown report.",
                                    )
                                })
                        })
                })
            });
        let task_input = if let Some(observer) = observer {
            let event_owner = owner.clone();
            let emit: super::observer::ObserverEmitter = Arc::new(move |event| {
                event_owner
                    .push_raw(event, None)
                    .map_err(|error| error.message)
            });
            let now: super::observer::ObserverClock =
                Arc::new(|| {
                    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                        Ok(value) => value.as_millis() as f64,
                        Err(value) => -(value.duration().as_millis() as f64),
                    }
                });
            super::observer::prepare_task(observer, fork, Some(emit), now).map_err(error)?
        } else {
            let request = super::observer::ObserverForkRequest {
                definition_id: definition.as_str().into(),
                definition: spec,
                directive,
                subagent_id: id.clone(),
                timeout_ms: Some(timeout),
                suppress_auto_observer: false,
                native_observer: false,
            };
            StartBackgroundSubagentInput {
                kind: None,
                session_id: self.turn.parent.session_id.clone(),
                parent_turn_id: self.turn.parent.turn_id.clone(),
                subagent_id: id.clone(),
                subagent_type: definition.as_str().into(),
                observed_subagent_id: None,
                run: Box::new(move |signal| {
                    Box::pin(async move { fork(request, signal).await.map(|_| ()) })
                }),
            }
        };
        let task = owner.start(runtime, task_input)?;
        record.commit();
        Ok(background_output(
            &output_definition,
            &output_description,
            &output_id,
            &task.task_id,
        ))
    }
}
async fn observer_slot(
    slots: Arc<Semaphore>,
    signal: &crate::runtime::tools::ToolCancellation,
    deadline: tokio::time::Instant,
) -> Result<tokio::sync::OwnedSemaphorePermit, ToolError> {
    tokio::select! {
        biased;
        _=signal.cancelled()=>Err(ToolError::aborted()),
        _=tokio::time::sleep_until(deadline)=>Err(error("Subagent timed out before starting.")),
        permit=slots.acquire_owned()=>permit.map_err(|_|error("Native subagent capacity is closed.")),
    }
}
fn detached_context(
    fields: JsObject,
    read: SharedFileState,
    write: SharedFileState,
    signal: crate::runtime::tools::ToolCancellation,
) -> ToolRunContext {
    use crate::host::modules::tool_context::{ToolFileState, ToolRuntimeContext};
    let host = Arc::new(ToolRuntimeContext {
        fields: fields.clone(),
        files: ToolFileState {
            read_file_state: read,
            write_snapshots: write,
            allowed_read_files: vec![],
        },
        abort_signal: Some(Arc::new(signal)),
        model: None,
        elicitation: None,
        user_dialog: None,
        audit_recorder: None,
        file_history: None,
        file_update_notifier: None,
        now: None,
        progress: None,
        subagent: None,
        plan_todo: None,
        goal: None,
        plan_directory: None,
    });
    ToolRunContext {
        host,
        fields,
        execute_tool: None,
    }
}
fn background_output(
    definition: &str,
    description: &JsString,
    id: &JsString,
    task: &JsString,
) -> ToolOutput {
    let text = concat(&[
        &"Background subagent started as task ".into(),
        task,
        &".".into(),
    ]);
    let data = object([
        ("subagentType", definition.into()),
        ("description", JsValue::String(description.clone())),
        ("text", JsValue::String(text.clone())),
        ("backgroundTaskId", JsValue::String(task.clone())),
    ])
    .as_object()
    .unwrap()
    .clone();
    ToolOutput {
        data: Some(JsValue::Object(data.clone())),
        content: vec![
            object([
                ("type", "text".into()),
                (
                    "text",
                    JsValue::String(concat(&[
                        &format!("[{definition}] ").into(),
                        description,
                        &"\n\n".into(),
                        &text,
                    ])),
                ),
            ]),
            object([("type", "json".into()), ("value", JsValue::Object(data))]),
        ],
        metadata: Some(
            object([
                ("subagent", definition.into()),
                ("subagentId", JsValue::String(id.clone())),
                ("backgroundTaskId", JsValue::String(task.clone())),
                ("forkMode", "background".into()),
            ])
            .as_object()
            .unwrap()
            .clone(),
        ),
        ..Default::default()
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use crate::runtime::{
        background::{BackgroundSubagentRuntime, StartBackgroundSubagentInput, TaskStatus},
        project::{ProjectRuntimeRegistry, ResourceBundle},
    };
    #[test]
    fn actual_ts_background_tool_output_matches() {
        let fixture =
            pilotdeck_protocol::parse_json(include_str!("agent_background_oracle.json")).unwrap();
        let JsValue::Array(cases) = fixture.get("cases").unwrap() else {
            panic!("cases")
        };
        let case = &cases[0];
        let output = background_output(
            "research",
            &"fixture task".into(),
            &"child-fixed".into(),
            &"task-fixed".into(),
        );
        let actual = object([
            ("content", JsValue::Array(output.content)),
            ("data", output.data.unwrap()),
            ("metadata", JsValue::Object(output.metadata.unwrap())),
        ]);
        assert_eq!(&actual, case.get("result").unwrap());
    }
    #[test]
    fn background_record_budget_is_bounded_and_failed_admission_rolls_back() {
        let registry = NativeForkRegistry::new();
        drop(BackgroundRecordReservation::reserve(registry.clone()).unwrap());
        assert_eq!(registry.background_records.load(Ordering::SeqCst), 0);
        registry.background_records.store(4095, Ordering::SeqCst);
        BackgroundRecordReservation::reserve(registry.clone())
            .unwrap()
            .commit();
        assert!(BackgroundRecordReservation::reserve(registry.clone()).is_err());
        assert_eq!(registry.background_records.load(Ordering::SeqCst), 4096);
    }
    #[tokio::test]
    async fn session_drain_cancels_all_tasks_and_waits_real_runtime_done_before_releasing_generation()
     {
        let registry = ProjectRuntimeRegistry::new(
            "/fixture-background-owner",
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        );
        let owner = NativeForkOwner::new(registry.acquire(None).unwrap());
        let runtime = BackgroundSubagentRuntime::default();
        let (cancelled, seen) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        let (started, entered) = tokio::sync::oneshot::channel();
        let task = owner
            .start(
                runtime.clone(),
                StartBackgroundSubagentInput {
                    kind: None,
                    session_id: "parent".into(),
                    parent_turn_id: "parent-turn".into(),
                    subagent_id: "child".into(),
                    subagent_type: "research".into(),
                    observed_subagent_id: None,
                    run: Box::new(move |signal| {
                        Box::pin(async move {
                            let _ = started.send(());
                            signal.cancelled().await;
                            let _ = cancelled.send(());
                            let _ = held.await;
                            Ok(())
                        })
                    }),
                },
            )
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        let (peer_started, peer_entered) = tokio::sync::oneshot::channel();
        let (peer_cancelled, peer_seen) = tokio::sync::oneshot::channel();
        let peer = owner
            .start(
                runtime.clone(),
                StartBackgroundSubagentInput {
                    kind: None,
                    session_id: "parent".into(),
                    parent_turn_id: "parent-turn".into(),
                    subagent_id: "peer".into(),
                    subagent_type: "research".into(),
                    observed_subagent_id: None,
                    run: Box::new(move |signal| {
                        Box::pin(async move {
                            let _ = peer_started.send(());
                            signal.cancelled().await;
                            let _ = peer_cancelled.send(());
                            Ok(())
                        })
                    }),
                },
            )
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), peer_entered)
            .await
            .unwrap()
            .unwrap();
        let mut draining = owner.clone().drain();
        tokio::time::timeout(std::time::Duration::from_secs(2), seen)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), peer_seen)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime.get(&peer.task_id).unwrap().status,
            TaskStatus::Cancelled
        );
        assert!(!owner.is_open());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut draining)
                .await
                .is_err()
        );
        assert_eq!(
            runtime.get(&task.task_id).unwrap().status,
            TaskStatus::Cancelled
        );
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), draining)
            .await
            .unwrap()
            .unwrap();
        owner.release_generation().await.unwrap();
        registry.dispose().await.unwrap();
    }
}

#[cfg(test)]
mod owned_inbox_job_tests {
    use super::*;
    use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle};
    #[tokio::test]
    async fn inbox_jobs_cancel_together_and_drain_retains_their_actual_completion() {
        let registry = ProjectRuntimeRegistry::new(
            "/fixture-inbox-owner",
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        );
        let owner = NativeForkOwner::new(registry.acquire(None).unwrap());
        let (started, entered) = tokio::sync::oneshot::channel();
        let (cancelled, seen) = tokio::sync::oneshot::channel();
        let (release, held) = tokio::sync::oneshot::channel();
        owner
            .spawn_owned(move |signal| {
                Box::pin(async move {
                    let _ = started.send(());
                    signal.cancelled().await;
                    let _ = cancelled.send(());
                    let _ = held.await;
                    Ok(())
                })
            })
            .unwrap();
        entered.await.unwrap();
        let (peer_started, peer_entered) = tokio::sync::oneshot::channel();
        let (peer_cancelled, peer_seen) = tokio::sync::oneshot::channel();
        owner
            .spawn_owned(move |signal| {
                Box::pin(async move {
                    let _ = peer_started.send(());
                    signal.cancelled().await;
                    let _ = peer_cancelled.send(());
                    Ok(())
                })
            })
            .unwrap();
        peer_entered.await.unwrap();
        let mut drain = owner.clone().drain();
        tokio::time::timeout(std::time::Duration::from_secs(2), seen)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), peer_seen)
            .await
            .unwrap()
            .unwrap();
        assert!(!owner.is_open());
        assert!(owner.spawn_owned(|_| Box::pin(async { Ok(()) })).is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut drain)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), drain)
            .await
            .unwrap()
            .unwrap();
        owner.release_generation().await.unwrap();
        registry.dispose().await.unwrap();
    }
}

#[cfg(test)]
mod observer_slot_tests {
    use super::*;
    use crate::runtime::tools::ToolCancellation;
    use std::{future::Future, task::Poll, time::Duration};
    #[tokio::test]
    async fn one_slot_observer_waits_until_observed_fork_releases_its_permit() {
        let slots = Arc::new(Semaphore::new(1));
        let observed = slots.clone().acquire_owned().await.unwrap();
        let signal = ToolCancellation::default();
        let mut observer = Box::pin(observer_slot(
            slots.clone(),
            &signal,
            tokio::time::Instant::now() + Duration::from_secs(2),
        ));
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(observer.as_mut().poll(cx).is_pending())).await
        );
        assert_eq!(slots.available_permits(), 0);
        drop(observed);
        let permit = tokio::time::timeout(Duration::from_secs(1), observer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slots.available_permits(), 0);
        drop(permit);
        assert_eq!(slots.available_permits(), 1);
    }
    #[tokio::test]
    async fn blocked_one_slot_observer_cancellation_and_deadline_do_not_take_the_permit() {
        let slots = Arc::new(Semaphore::new(1));
        let observed = slots.clone().acquire_owned().await.unwrap();
        let signal = ToolCancellation::default();
        let mut observer = Box::pin(observer_slot(
            slots.clone(),
            &signal,
            tokio::time::Instant::now() + Duration::from_secs(2),
        ));
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(observer.as_mut().poll(cx).is_pending())).await
        );
        signal.abort();
        let failure = tokio::time::timeout(Duration::from_secs(1), observer)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(failure.code, ToolError::aborted().code);
        let signal = ToolCancellation::default();
        let failure = tokio::time::timeout(
            Duration::from_secs(1),
            observer_slot(
                slots.clone(),
                &signal,
                tokio::time::Instant::now() + Duration::from_millis(10),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            failure.message,
            "Subagent timed out before starting.".into()
        );
        assert_eq!(slots.available_permits(), 0);
        drop(observed);
        assert_eq!(slots.available_permits(), 1);
    }
}
