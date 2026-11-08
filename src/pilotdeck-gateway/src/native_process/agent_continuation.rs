//! Continuable agents own real Factory handles and the existing durable inbox.
//! This module owns admission/pumps only; child policy and loop assembly stay shared.
use super::*;
use crate::runtime::{
    project::RuntimeResult,
    session::{
        HandleState,
        factory::{SessionBinding, SessionProjection},
        inbox::QueuedTurn,
    },
};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicUsize},
};
use tokio::sync::{Mutex as AsyncMutex, Notify};

fn tool_message(error: &ToolError) -> String {
    String::from_utf16_lossy(error.message.units())
}
pub(in crate::native_process) type ResumeAuthority = Arc<
    dyn Fn(
            Option<TurnInteractionAuthority>,
        ) -> crate::runtime::refresh::BoxFuture<
            Result<Option<TurnInteractionAuthority>, RuntimeError>,
        > + Send
        + Sync,
>;
type Lookup = Arc<dyn Fn(&JsString) -> Option<LiveSessionHandle> + Send + Sync>;
#[derive(Clone)]
struct Binding {
    recorder: Weak<Recorder>,
    projection: Weak<SessionProjection>,
    owner: Weak<NativeForkOwner>,
    resume_authority: ResumeAuthority,
}
#[derive(Default)]
pub(super) struct Continuations {
    lookup: OnceLock<Lookup>,
    bindings: Mutex<HashMap<JsString, Binding>>,
    active: Mutex<HashMap<JsString, Weak<Activation>>>,
    gates: Mutex<HashMap<JsString, Weak<AsyncMutex<()>>>>,
    parent_pumps: Mutex<HashMap<JsString, Weak<ParentPump>>>,
}
struct Activation {
    session: JsString,
    handle: LiveSessionHandle,
    binding: Binding,
    parent: Option<LiveSessionHandle>,
    parent_activation: Option<Arc<Activation>>,
    definition: String,
    child_mode: Option<JsString>,
    api: Arc<ForkApi>,
    gate: Arc<AsyncMutex<()>>,
    wake: Notify,
    closing: AtomicBool,
    announced: AtomicBool,
    children: AtomicUsize,
    done: watch::Sender<Option<RuntimeResult>>,
}
impl NativeForkRegistry {
    pub(in crate::native_process) fn install_session_lookup(&self, lookup: Lookup) {
        assert!(
            self.continuations.lookup.set(lookup).is_ok(),
            "session lookup is installed once"
        );
    }
    pub(in crate::native_process) fn bind_session(
        &self,
        binding: &SessionBinding,
        owner: &Arc<NativeForkOwner>,
        resume_authority: ResumeAuthority,
    ) -> RuntimeResult {
        let mut bindings = lock(&self.continuations.bindings);
        bindings.retain(|_, value| value.owner.strong_count() > 0);
        if bindings.len() >= 4096 && !bindings.contains_key(&binding.context.session_key) {
            return Err(RuntimeError::new(
                "Native continuation binding budget is full.",
            ));
        }
        bindings.insert(
            binding.context.session_key.clone(),
            Binding {
                recorder: Arc::downgrade(&binding.recorder),
                projection: Arc::downgrade(&binding.projection),
                owner: Arc::downgrade(owner),
                resume_authority,
            },
        );
        Ok(())
    }
    fn binding(&self, key: &JsString) -> Result<Binding, ToolError> {
        lock(&self.continuations.bindings)
            .get(key)
            .cloned()
            .filter(|binding| binding.owner.upgrade().is_some_and(|owner| owner.is_open()))
            .ok_or_else(|| error("Continuable agent session is no longer bound."))
    }
    fn activation(&self, key: &JsString) -> Option<Arc<Activation>> {
        lock(&self.continuations.active)
            .get(key)
            .and_then(Weak::upgrade)
    }
    fn live_handle(&self, key: &JsString) -> Option<LiveSessionHandle> {
        if let Some(active) = self.activation(key)
            && !active.closing.load(Ordering::SeqCst)
            && active.handle.state() == HandleState::Active
        {
            return Some(active.handle.clone());
        }
        self.continuations
            .lookup
            .get()
            .and_then(|lookup| lookup(key))
    }
    fn gate(&self, key: &JsString) -> Arc<AsyncMutex<()>> {
        let mut gates = lock(&self.continuations.gates);
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(key).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(AsyncMutex::new(()));
        gates.insert(key.clone(), Arc::downgrade(&gate));
        gate
    }
}

fn admission_output(child: &JsString, item: &QueuedTurn) -> ToolOutput {
    ToolOutput {
        data: Some(object([
            ("subagentId", JsValue::String(child.clone())),
            ("messageId", JsValue::String(item.item_id.clone())),
            ("turnId", JsValue::String(item.turn_id.clone())),
        ])),
        content: vec![object([
            ("type", "text".into()),
            (
                "text",
                JsValue::String(concat(&[
                    &"accepted message ".into(),
                    &item.item_id,
                    &" for subagent ".into(),
                    child,
                ])),
            ),
        ])],
        ..Default::default()
    }
}
fn queued(text: JsString) -> QueuedTurn {
    QueuedTurn {
        item_id: uuid::Uuid::new_v4().to_string().into(),
        turn_id: uuid::Uuid::new_v4().to_string().into(),
        input: object([("type", "text".into()), ("text", JsValue::String(text))]),
        submit_options: JsObject::new(),
    }
}
fn require_string(input: &JsValue, field: &str) -> Result<JsString, ToolError> {
    input
        .get(field)
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| ToolError::new("invalid_tool_input", format!("{field} must be a string.")))
}
fn require_nonempty(value: &JsString, field: &str) -> Result<(), ToolError> {
    let whitespace = |unit: &u16| matches!(*unit,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff);
    if value.units().iter().all(whitespace) {
        return Err(error(format!("{field} must not be empty.")));
    }
    Ok(())
}

fn ensure_live(api: &ForkApi, parent: &LiveSessionHandle) -> Result<(), ToolError> {
    let registry = &api.turn.registry;
    let current = registry
        .live_handle(&api.turn.parent.session_id)
        .filter(|current| current.same_identity(parent));
    let binding = registry.binding(&api.turn.parent.session_id)?;
    let actual = binding.owner.upgrade().ok_or_else(ToolError::aborted)?;
    let expected = api
        .turn
        .owner
        .as_ref()
        .and_then(Weak::upgrade)
        .ok_or_else(ToolError::aborted)?;
    if current.is_none() || !Arc::ptr_eq(&actual, &expected) {
        return Err(error(concat(&[
            &"Continuable subagent operation requires the exact live parent agent: ".into(),
            &api.turn.parent.session_id,
        ])));
    }
    Ok(())
}
async fn append(item: &QueuedTurn, session: &JsString, binding: &Binding) -> Result<(), ToolError> {
    let recorder = binding
        .recorder
        .upgrade()
        .ok_or_else(|| error("Agent inbox recorder is closed."))?;
    let projection = binding
        .projection
        .upgrade()
        .ok_or_else(|| error("Agent inbox projection is closed."))?;
    let pending = projection
        .pending_turns(session)
        .map_err(|failure| error(failure.to_string()))?;
    if pending.len() >= 32 {
        return Err(error("Native agent inbox item budget is full."));
    }
    let bytes = pilotdeck_protocol::stringify_json(&item.input)
        .map_err(|failure| error(failure.to_string()))?
        .map_or(0, |value| value.len());
    if bytes > 1024 * 1024 {
        return Err(error("Native agent inbox input exceeds one MiB."));
    }
    let total = pending.iter().try_fold(bytes, |total, item| {
        let value = pilotdeck_protocol::stringify_json(&item.input)
            .map_err(|failure| error(failure.to_string()))?
            .map_or(0, |value| value.len());
        total
            .checked_add(value)
            .ok_or_else(|| error("Native agent inbox byte budget is full."))
    })?;
    if total > 4 * 1024 * 1024 {
        return Err(error("Native agent inbox byte budget is full."));
    }
    let item = item.clone();
    let session = session.clone();
    tokio::task::spawn_blocking(move || item.enqueue(&recorder, &session))
        .await
        .map_err(|failure| error(failure.to_string()))?
        .map_err(|failure| error(failure.to_string()))
}

impl ForkApi {
    pub(super) async fn continue_input(
        self: Arc<Self>,
        input: JsValue,
        context: ToolRunContext,
        followup: bool,
    ) -> Result<ToolOutput, ToolError> {
        // Admission survives the consumer dropping its future, just like one-shot
        // creation. The owning turn waits this task through its close path.
        let mut wait = {
            let closed = lock(&self.turn.closed);
            if *closed || context.is_aborted() {
                return Err(ToolError::aborted());
            }
            let id = self.turn.next.fetch_add(1, Ordering::SeqCst);
            let cancel = Cancellation::new();
            let (done, receiver) = watch::channel(None);
            lock(&self.turn.jobs).insert(
                id,
                Job {
                    cancel: cancel.clone(),
                    done: receiver.clone(),
                },
            );
            let guard = JobGuard {
                turn: self.turn.clone(),
                id,
                result: done,
            };
            let run_cancel = cancel.clone();
            let api = self.clone();
            tokio::spawn(async move {
                let guard = guard;
                let result = api
                    .continuation_admit(input, context, followup, run_cancel)
                    .await;
                guard.result.send_replace(Some(result));
            });
            drop(closed);
            JobWait {
                cancel,
                done: receiver,
            }
        };
        wait.done
            .wait_for(Option::is_some)
            .await
            .map_err(|_| error("Continuation admission stopped."))?
            .as_ref()
            .unwrap()
            .clone()
    }
    async fn continuation_admit(
        self: Arc<Self>,
        input: JsValue,
        context: ToolRunContext,
        followup: bool,
        cancel: Cancellation,
    ) -> Result<ToolOutput, ToolError> {
        let registry = self.turn.registry.clone();
        let parent = registry
            .live_handle(&self.turn.parent.session_id)
            .ok_or_else(|| error("Continuable subagent requires a live parent agent."))?;
        ensure_live(&self, &parent)?;
        let child = if followup {
            require_string(&input, "subagent_id")?
        } else {
            uuid::Uuid::new_v4().to_string().into()
        };
        require_nonempty(&child, "childSessionId")?;
        // Returned ids are UUIDs. Refuse arbitrary path-like ids before asking
        // an application store for its sidechain namespace.
        let child_utf8 = child
            .to_utf8()
            .map_err(|_| error("Invalid continuable subagent id."))?;
        if uuid::Uuid::parse_str(&child_utf8).is_err() {
            return Err(error("Invalid continuable subagent id."));
        }
        let item = queued(require_string(
            &input,
            if followup { "message" } else { "prompt" },
        )?);
        let gate = registry.gate(&child);
        let _admission = gate.lock().await;
        ensure_live(&self, &parent)?;
        if cancel.is_cancelled() || context.is_aborted() {
            return Err(ToolError::aborted());
        }
        if let Some(active) = registry.activation(&child) {
            if !active
                .parent
                .as_ref()
                .is_some_and(|owner| owner.same_identity(&parent))
            {
                return Err(error(concat(&[
                    &"Continuable subagent ".into(),
                    &child,
                    &" belongs to another live parent agent.".into(),
                ])));
            }
            if active.closing.load(Ordering::SeqCst) {
                let mut done = active.done.subscribe();
                drop(_admission);
                loop {
                    let result = done.borrow_and_update().clone();
                    if let Some(result) = result {
                        result.map_err(|failure| error(failure.to_string()))?;
                        break;
                    }
                    done.changed()
                        .await
                        .map_err(|_| error("Continuation release notification stopped."))?;
                }
                // Re-enter through the same serial gate after the previous
                // writer has really closed, rather than opening it in parallel.
                drop(done);
                drop(active);
                return Box::pin(self.continuation_admit(input, context, followup, cancel)).await;
            }
            append(&item, &child, &active.binding).await?;
            active.wake.notify_one();
            return Ok(admission_output(&child, &item));
        }

        let definition;
        let mut spec;
        let descriptor;
        if followup {
            let (store, key, _) = self.turn.recorder.writer().sidechain_storage(&child);
            let loaded =
                tokio::task::spawn_blocking(move || store.read(&key, Some(50 * 1024 * 1024)))
                    .await
                    .map_err(|failure| error(failure.to_string()))?
                    .map_err(|_| {
                        error(concat(&[
                            &"Continuable subagent is not resumable: ".into(),
                            &child,
                        ]))
                    })?;
            if loaded.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == crate::storage::Diagnostic::TRANSCRIPT_TOO_LARGE
            }) {
                return Err(error(
                    "Continuable child transcript exceeds the supported read budget.",
                ));
            }
            descriptor = persisted_descriptor(&loaded.entries, &child)?;
            if string(&descriptor, "parentSessionId").as_ref() != Some(&self.turn.parent.session_id)
            {
                return Err(error(concat(&[
                    &"Continuable subagent ".into(),
                    &child,
                    &" belongs to another parent session.".into(),
                ])));
            }
            definition = string(&descriptor, "definitionId")
                .unwrap()
                .to_utf8()
                .map_err(|_| error("Unknown persisted subagent type."))?;
            spec = definitions(&JsObject::new())
                .get(definition.as_str())
                .and_then(JsValue::as_object)
                .cloned()
                .ok_or_else(|| error(format!("Unknown persisted subagent type: {definition}")))?;
            spec.insert(
                "modelOverride".into(),
                object([
                    ("provider", descriptor.get("agentProvider").unwrap().clone()),
                    ("model", descriptor.get("agentModel").unwrap().clone()),
                ]),
            );
        } else {
            let label = require_string(&input, "description")?;
            require_nonempty(&label, "label")?;
            let id = input
                .get("subagent_type")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| "general-purpose".into());
            definition = id.to_utf8().map_err(|_| error("Unknown subagent type."))?;
            spec = definitions(&JsObject::new())
                .get(definition.as_str())
                .and_then(JsValue::as_object)
                .cloned()
                .ok_or_else(|| error(concat(&[&"Unknown subagent type: ".into(), &id])))?;
            let selected = self
                .turn
                .config
                .get("subagentModel")
                .and_then(JsValue::as_object)
                .unwrap_or(&self.turn.config);
            descriptor = object([
                ("version", JsValue::Number(2.0)),
                ("mode", "continuable".into()),
                ("provider", "pilotdeck-native".into()),
                ("definitionId", JsValue::String(id)),
                (
                    "parentSessionId",
                    JsValue::String(self.turn.parent.session_id.clone()),
                ),
                ("label", JsValue::String(label)),
                (
                    "agentProvider",
                    JsValue::String(
                        string(selected, "provider")
                            .ok_or_else(|| error("Continuable agent provider is missing."))?,
                    ),
                ),
                (
                    "agentModel",
                    JsValue::String(
                        string(selected, "model")
                            .ok_or_else(|| error("Continuable agent model is missing."))?,
                    ),
                ),
            ])
            .as_object()
            .unwrap()
            .clone();
        }
        let depth = self
            .turn
            .config
            .get("subagentDepth")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let max = self
            .turn
            .config
            .get("maxSubagentDepth")
            .and_then(JsValue::as_number)
            .unwrap_or(1.0);
        if depth >= max {
            return Err(fork_error(
                "Nested fork depth exceeded.",
                "subagent_depth_exceeded",
            ));
        }
        let parent_mode = context
            .fields
            .get("permissionContext")
            .and_then(JsValue::as_object)
            .and_then(|value| string(value, "mode"))
            .or_else(|| string(&self.turn.config, "permissionMode"))
            .unwrap_or_else(|| "default".into());
        let parent_ask = context
            .fields
            .get("runMode")
            .and_then(JsValue::as_string)
            .is_some_and(|mode| mode.equals_str("ask"));
        let permit = registry
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| error("Native subagent capacity is full."))?;
        let factory = registry
            .factory
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| error("Native session factory is unavailable."))?;
        let (scope, child_context, _) = self.compose_child(
            &definition,
            &spec,
            &parent_mode,
            parent_ask,
            depth,
            max,
            &child,
            &child,
            Some(descriptor.clone()),
            !followup,
        )?;
        let child_mode = scope.permission_mode.clone();
        lock(&registry.scopes).insert(child.clone(), scope);
        let scope_guard = ScopeGuard {
            registry: registry.clone(),
            key: child.clone(),
        };
        let owner = self
            .turn
            .owner
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(ToolError::aborted)?;
        let detached = self.detached_for_continuation(&owner)?;
        // Await a real construction even after cancellation. Its writer cleanup
        // remains owned; no timeout claims to interrupt a blocked loader.
        let handle = factory
            .create(child_context, None)
            .await
            .map_err(|failure| error(failure.to_string()))?;
        drop(scope_guard);
        let binding = match registry.binding(&child) {
            Ok(binding) => binding,
            Err(failure) => {
                handle
                    .dispose("continuable_subagent_admission_rollback".into())
                    .await
                    .map_err(|cleanup| {
                        error(concat(&[
                            &failure.message,
                            &"; ".into(),
                            &cleanup.to_string().into(),
                        ]))
                    })?;
                return Err(failure);
            }
        };
        if cancel.is_cancelled() || context.is_aborted() || ensure_live(&self, &parent).is_err() {
            handle
                .dispose("continuable_subagent_admission_rollback".into())
                .await
                .map_err(|failure| error(failure.to_string()))?;
            return Err(ToolError::aborted());
        }
        let parent_activation = registry.activation(&self.turn.parent.session_id);
        if let Some(parent) = &parent_activation {
            if parent.closing.load(Ordering::SeqCst) {
                handle
                    .dispose("continuable_subagent_admission_rollback".into())
                    .await
                    .map_err(|failure| error(failure.to_string()))?;
                return Err(ToolError::aborted());
            }
            parent.children.fetch_add(1, Ordering::SeqCst);
        }
        let active = Arc::new(Activation {
            session: child.clone(),
            handle,
            binding,
            parent: Some(parent),
            parent_activation,
            definition,
            child_mode: Some(child_mode),
            api: detached,
            gate: gate.clone(),
            wake: Notify::new(),
            closing: AtomicBool::new(false),
            announced: AtomicBool::new(false),
            children: AtomicUsize::new(0),
            done: watch::channel(None).0,
        });
        lock(&registry.continuations.active).insert(child.clone(), Arc::downgrade(&active));
        let admission = async {
            if !followup {
                let recorder = active
                    .binding
                    .recorder
                    .upgrade()
                    .ok_or_else(|| error("Child descriptor recorder closed."))?;
                let child = child.clone();
                let turn = item.turn_id.clone();
                tokio::task::spawn_blocking(move || {
                    recorder.writer().record_session_event_checked(
                        &child,
                        &turn,
                        object([
                            ("type", "subagent_descriptor".into()),
                            ("descriptor", JsValue::Object(descriptor)),
                        ])
                        .as_object()
                        .unwrap()
                        .clone(),
                    )
                })
                .await
                .map_err(|failure| error(failure.to_string()))?
                .map_err(|failure| error(failure.to_string()))?;
            }
            ensure_live(&self, active.parent.as_ref().unwrap())?;
            if cancel.is_cancelled() || context.is_aborted() {
                return Err(ToolError::aborted());
            }
            append(&item, &child, &active.binding).await?;
            active.announced.store(true, Ordering::SeqCst);
            Ok(())
        }
        .await;
        if let Err(failure) = admission {
            active.closing.store(true, Ordering::SeqCst);
            drop(_admission);
            active.close(false, Some(tool_message(&failure))).await?;
            return Err(failure);
        }
        let runner = active.clone();
        let completion = crate::runtime::cleanup_task::Completion::new(
            active.done.clone(),
            RuntimeError::new("Continuable child pump was dropped or panicked."),
        );
        if let Err(failure) = owner.spawn_owned(move |cancel| {
            Box::pin(async move {
                let _permit = permit;
                let result = runner.clone().pump(cancel).await;
                completion.finish(result.clone());
                result
            })
        }) {
            active.closing.store(true, Ordering::SeqCst);
            drop(_admission);
            active.close(false, Some(tool_message(&failure))).await?;
            return Err(failure);
        }
        Ok(admission_output(&child, &item))
    }

    fn detached_for_continuation(
        &self,
        owner: &Arc<NativeForkOwner>,
    ) -> Result<Arc<Self>, ToolError> {
        let mut authority = self.turn.authority.clone();
        if let Some(authority) = &mut authority {
            authority.emit = self
                .turn
                .session_emit
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(|_| false));
        }
        let mut turn = ForkTurn::new(
            self.turn.registry.clone(),
            self.turn.parent.clone(),
            self.turn.config.clone(),
            self.turn.recorder.clone(),
            self.turn.rules.clone(),
            authority,
            None,
            self.turn.tools.clone(),
            self.turn.skills.clone(),
            Some(owner),
            #[cfg(unix)]
            self.turn.sandbox.clone(),
        );
        if let Some(emit) = self.turn.session_emit.get() {
            turn.bind_session_emitter(emit.clone());
        }
        Arc::get_mut(&mut turn).unwrap().detached = true;
        turn.bind_generation(
            self.turn
                .generation
                .get()
                .cloned()
                .ok_or_else(|| error("Parent execution world is unavailable."))?,
        )
        .map_err(|failure| error(failure.to_string()))?;
        Ok(Arc::new(Self {
            turn,
            read: Arc::new(Mutex::new(lock(&self.read).clone())),
            write: Arc::new(Mutex::new(lock(&self.write).clone())),
        }))
    }
}

impl Activation {
    async fn pump(self: Arc<Self>, cancel: Cancellation) -> RuntimeResult {
        let mut failure = None;
        loop {
            let wake = self.wake.notified();
            let gate = self.gate.lock().await;
            if cancel.is_cancelled() {
                break;
            }
            let Some(projection) = self.binding.projection.upgrade() else {
                failure = Some("Agent inbox projection closed.".into());
                break;
            };
            let next = match projection.pending_turns(&self.session) {
                Ok(pending) => pending.front().cloned(),
                Err(error) => {
                    failure = Some(error.to_string());
                    break;
                }
            };
            if let Some(item) = next {
                drop(gate);
                if let Err(error) = self.drive(item, &cancel).await {
                    failure = Some(tool_message(&error));
                    break;
                }
            } else if self.children.load(Ordering::SeqCst) > 0 {
                drop(gate);
                tokio::select! { _=cancel.cancelled()=>break, _=wake=>{} }
            } else {
                self.closing.store(true, Ordering::SeqCst);
                drop(gate);
                break;
            }
        }
        self.close(true, failure)
            .await
            .map_err(|failure| RuntimeError::new(tool_message(&failure)))
    }
    async fn drive(&self, item: QueuedTurn, cancel: &Cancellation) -> Result<(), ToolError> {
        tokio::select! {
            _=cancel.cancelled()=>return Err(ToolError::aborted()),
            result=self.handle.when_idle()=>result.map_err(|failure|error(failure.to_string()))?,
        }
        let mut options = item.submit_options.clone();
        options.insert("sessionId".into(), JsValue::String(self.session.clone()));
        options.insert("turnId".into(), JsValue::String(item.turn_id));
        options.insert("runId".into(), options.get("turnId").unwrap().clone());
        let mut authority = self.api.turn.authority.clone();
        if let Some(authority) = &mut authority {
            // The selected child mode belongs to the child's composed session.
            authority.permission_mode = self.child_mode.clone();
        }
        let mut stream = self
            .handle
            .submit_queued(
                item.input,
                JsValue::Object(options),
                authority,
                item.item_id,
            )
            .map_err(|failure| error(failure.to_string()))?;
        let mut failure = None;
        let mut terminal = false;
        loop {
            let next = if failure.is_some() {
                stream.next().await
            } else {
                tokio::select! {
                    _=cancel.cancelled()=>{
                        let _ = self.handle.abort(&"parent_agent_disposed".into());
                        failure=Some(ToolError::aborted()); continue;
                    },
                    next=stream.next()=>next,
                }
            };
            match next {
                Ok(None) => break,
                Err(error) => {
                    failure = Some(super::error(error.to_string()));
                    break;
                }
                Ok(Some(event)) => {
                    if event
                        .get("type")
                        .and_then(JsValue::as_string)
                        .is_some_and(|kind| kind.equals_str("turn_completed"))
                    {
                        terminal = true;
                    }
                    if failure.is_none()
                        && let Err(error) = self
                            .api
                            .forward_event(&event, &self.session, &self.definition)
                            .await
                    {
                        let _ = self.handle.abort(&"subagent_delivery_failed".into());
                        failure = Some(error);
                    }
                }
            }
        }
        drop(stream);
        if let Some(failure) = failure {
            return Err(failure);
        }
        if !terminal {
            return Err(error(
                "Continuable child loop ended without a terminal result.",
            ));
        }
        Ok(())
    }
    async fn close(
        self: &Arc<Self>,
        notify_parent: bool,
        failure: Option<String>,
    ) -> Result<(), ToolError> {
        let gate = self.gate.lock().await;
        self.closing.store(true, Ordering::SeqCst);
        let mut errors = Vec::new();
        if let (Some(recorder), Some(projection)) = (
            self.binding.recorder.upgrade(),
            self.binding.projection.upgrade(),
        ) {
            match projection.pending_turns(&self.session) {
                Ok(pending) => {
                    let session = self.session.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        for item in pending {
                            item.discard(&recorder, &session)?;
                        }
                        Ok::<_, RuntimeError>(())
                    })
                    .await;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => errors.push(error.to_string()),
                        Err(error) => errors.push(error.to_string()),
                    }
                }
                Err(error) => errors.push(error.to_string()),
            }
        } else {
            errors.push("Continuation inbox closed before its owned drain.".into());
        }
        drop(gate);
        let state = match failure.as_ref() {
            Some(_) => "failed",
            None => match self
                .handle
                .snapshot()
                .get("status")
                .and_then(JsValue::as_string)
            {
                Some(status) if status.equals_str("idle") => "completed",
                Some(status) if status.equals_str("aborted") => "aborted",
                Some(status) if status.equals_str("failed") => "failed",
                _ => "unknown",
            },
        };
        if let Err(error) = self
            .handle
            .dispose("continuable_subagent_settled".into())
            .await
        {
            errors.push(error.to_string());
        }
        if notify_parent && self.announced.load(Ordering::SeqCst) {
            let state = if errors.is_empty() { state } else { "failed" };
            let detail = failure
                .or_else(|| errors.first().cloned())
                .map(|value| format!(" ({value})"))
                .unwrap_or_default();
            if let Err(error) = self
                .notify_parent(
                    format!(
                        "Continuable subagent {} settled: {state}{detail}.",
                        String::from_utf16_lossy(self.session.units())
                    )
                    .into(),
                )
                .await
            {
                errors.push(tool_message(&error));
            }
        }
        // Keep the ownership edge until the notice is durably admitted.
        if let Some(parent) = &self.parent_activation {
            parent.children.fetch_sub(1, Ordering::SeqCst);
            parent.wake.notify_one();
        }
        {
            let mut active = lock(&self.api.turn.registry.continuations.active);
            if active
                .get(&self.session)
                .and_then(Weak::upgrade)
                .is_some_and(|value| Arc::ptr_eq(&value, self))
            {
                active.remove(&self.session);
            }
        }
        let result = if errors.is_empty() {
            Ok(())
        } else {
            Err(RuntimeError::new(errors.join("; ")))
        };
        self.done.send_replace(Some(result.clone()));
        result.map_err(|failure| error(failure.to_string()))
    }
    async fn notify_parent(&self, text: JsString) -> Result<(), ToolError> {
        let Some(parent) = &self.parent else {
            return Ok(());
        };
        self.api
            .turn
            .registry
            .clone()
            .enqueue_parent(
                self.api.turn.parent.session_id.clone(),
                parent.clone(),
                text,
                self.api.turn.authority.clone(),
            )
            .await
    }
}

struct ParentPump {
    session: JsString,
    handle: LiveSessionHandle,
    binding: Binding,
    registry: Arc<NativeForkRegistry>,
    gate: Arc<AsyncMutex<()>>,
    seed_authority: Option<TurnInteractionAuthority>,
    closing: AtomicBool,
    done: watch::Sender<Option<RuntimeResult>>,
}
impl NativeForkRegistry {
    fn parent_pump(&self, key: &JsString) -> Option<Arc<ParentPump>> {
        lock(&self.continuations.parent_pumps)
            .get(key)
            .and_then(Weak::upgrade)
    }
    /// Called by the native factory after the exact session is published.
    pub(in crate::native_process) fn resume_published(
        self: &Arc<Self>,
        context: SessionContext,
        handle: LiveSessionHandle,
    ) -> RuntimeResult {
        let binding = self
            .binding(&context.session_key)
            .map_err(|error| RuntimeError::new(tool_message(&error)))?;
        let projection = binding
            .projection
            .upgrade()
            .ok_or_else(|| RuntimeError::new("Restored parent inbox projection is closed."))?;
        if projection.pending_turns(&context.session_key)?.is_empty() {
            return Ok(());
        }
        if self.activation(&context.session_key).is_some()
            || self.parent_pump(&context.session_key).is_some()
        {
            return Ok(());
        }
        self.start_parent_pump(context.session_key, handle, binding, None)
            .map_err(|error| RuntimeError::new(tool_message(&error)))
    }
    fn start_parent_pump(
        self: &Arc<Self>,
        session: JsString,
        handle: LiveSessionHandle,
        binding: Binding,
        seed_authority: Option<TurnInteractionAuthority>,
    ) -> Result<(), ToolError> {
        let owner = binding.owner.upgrade().ok_or_else(ToolError::aborted)?;
        let pump = Arc::new(ParentPump {
            gate: self.gate(&session),
            session: session.clone(),
            handle,
            binding,
            registry: self.clone(),
            seed_authority,
            closing: AtomicBool::new(false),
            done: watch::channel(None).0,
        });
        lock(&self.continuations.parent_pumps).insert(session, Arc::downgrade(&pump));
        let completion = crate::runtime::cleanup_task::Completion::new(
            pump.done.clone(),
            RuntimeError::new("Parent followup pump was dropped or panicked."),
        );
        owner.spawn_owned(move |cancel| {
            Box::pin(async move {
                let result = pump.clone().run(cancel).await;
                completion.finish(result.clone());
                result
            })
        })
    }
    async fn enqueue_parent(
        self: Arc<Self>,
        session: JsString,
        handle: LiveSessionHandle,
        text: JsString,
        seed_authority: Option<TurnInteractionAuthority>,
    ) -> Result<(), ToolError> {
        if handle.state() != HandleState::Active
            || !self
                .live_handle(&session)
                .is_some_and(|live| live.same_identity(&handle))
        {
            return Ok(());
        }
        let binding = self.binding(&session)?;
        let gate = self.gate(&session);
        let admission = gate.lock().await;
        if let Some(active) = self.activation(&session) {
            if active.closing.load(Ordering::SeqCst) {
                return Ok(());
            }
            append(&queued(text), &session, &binding).await?;
            active.wake.notify_one();
            return Ok(());
        }
        if let Some(pump) = self.parent_pump(&session) {
            if pump.closing.load(Ordering::SeqCst) {
                let mut done = pump.done.subscribe();
                drop(admission);
                loop {
                    let result = done.borrow_and_update().clone();
                    if let Some(result) = result {
                        result.map_err(|failure| error(failure.to_string()))?;
                        break;
                    }
                    done.changed()
                        .await
                        .map_err(|_| error("Parent followup release notification stopped."))?;
                }
                drop(done);
                drop(pump);
                return Box::pin(self.enqueue_parent(session, handle, text, seed_authority)).await;
            }
            append(&queued(text), &session, &binding).await?;
            return Ok(());
        }
        append(&queued(text), &session, &binding).await?;
        self.start_parent_pump(session, handle, binding, seed_authority)
    }
}
impl ParentPump {
    async fn run(self: Arc<Self>, cancel: Cancellation) -> RuntimeResult {
        let mut errors = Vec::new();
        loop {
            let gate = self.gate.lock().await;
            let pending = match self.binding.projection.upgrade() {
                Some(projection) => projection.pending_turns(&self.session),
                None => Err(RuntimeError::new("Parent inbox projection closed.")),
            };
            let next = match pending {
                Ok(pending) => pending.front().cloned(),
                Err(error) => {
                    errors.push(error.to_string());
                    break;
                }
            };
            if cancel.is_cancelled() || next.is_none() {
                self.closing.store(true, Ordering::SeqCst);
                drop(gate);
                break;
            }
            drop(gate);
            if let Err(error) = self.drive(next.unwrap(), &cancel).await {
                errors.push(tool_message(&error));
                break;
            }
        }
        self.closing.store(true, Ordering::SeqCst);
        if cancel.is_cancelled() || !errors.is_empty() {
            let _gate = self.gate.lock().await;
            if let (Some(recorder), Some(projection)) = (
                self.binding.recorder.upgrade(),
                self.binding.projection.upgrade(),
            ) {
                match projection.pending_turns(&self.session) {
                    Ok(pending) => {
                        let session = self.session.clone();
                        match tokio::task::spawn_blocking(move || {
                            for item in pending {
                                item.discard(&recorder, &session)?
                            }
                            Ok::<_, RuntimeError>(())
                        })
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => errors.push(error.to_string()),
                            Err(error) => errors.push(error.to_string()),
                        }
                    }
                    Err(error) => errors.push(error.to_string()),
                }
            } else {
                errors.push("Parent inbox closed before its owned drain.".into())
            }
        }
        // The published SessionOwner remains the sole ordinary-session disposer.
        {
            let mut pumps = lock(&self.registry.continuations.parent_pumps);
            if pumps
                .get(&self.session)
                .and_then(Weak::upgrade)
                .is_some_and(|pump| Arc::ptr_eq(&pump, &self))
            {
                pumps.remove(&self.session);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(RuntimeError::new(errors.join("; ")))
        }
    }
    async fn drive(&self, item: QueuedTurn, cancel: &Cancellation) -> Result<(), ToolError> {
        tokio::select! {
            _=cancel.cancelled()=>return Err(ToolError::aborted()),
            result=self.handle.when_idle()=>result.map_err(|error_|error(error_.to_string()))?,
        }
        let mut options = item.submit_options.clone();
        options.insert("sessionId".into(), JsValue::String(self.session.clone()));
        options.insert("turnId".into(), JsValue::String(item.turn_id.clone()));
        options.insert("runId".into(), JsValue::String(item.turn_id));
        // Read committed SDK configuration for this actual new turn. The
        // typed handle installs its own exact slot fence after reserving it.
        let authority = (self.binding.resume_authority)(self.seed_authority.clone())
            .await
            .map_err(|error_| error(error_.to_string()))?;
        if cancel.is_cancelled() {
            return Err(ToolError::aborted());
        }
        let mut stream = self
            .handle
            .submit_queued(
                item.input,
                JsValue::Object(options),
                authority,
                item.item_id,
            )
            .map_err(|error_| error(error_.to_string()))?;
        let owner = self
            .binding
            .owner
            .upgrade()
            .ok_or_else(ToolError::aborted)?;
        let mut failure = None;
        let mut terminal = false;
        loop {
            let next = if failure.is_some() {
                stream.next().await
            } else {
                tokio::select! {
                    _=cancel.cancelled()=>{
                        let _=self.handle.abort(&"parent_agent_disposed".into());
                        failure=Some(ToolError::aborted());continue
                    },
                    next=stream.next()=>next,
                }
            };
            match next {
                Ok(None) => break,
                Err(error_) => {
                    failure = Some(error(error_.to_string()));
                    break;
                }
                Ok(Some(event)) => {
                    if event
                        .get("type")
                        .and_then(JsValue::as_string)
                        .is_some_and(|kind| kind.equals_str("turn_completed"))
                    {
                        terminal = true
                    }
                    if failure.is_none()
                        && let Some(event) = event.as_object()
                        && let Err(error_) = owner.push_raw(event.clone(), None)
                    {
                        let _ = self.handle.abort(&"parent_followup_delivery_failed".into());
                        failure = Some(error_);
                    }
                }
            }
        }
        drop(stream);
        if let Some(failure) = failure {
            return Err(failure);
        }
        if !terminal {
            return Err(error(
                "Parent followup loop ended without a terminal result.",
            ));
        }
        Ok(())
    }
}

fn persisted_descriptor(entries: &[JsValue], child: &JsString) -> Result<JsObject, ToolError> {
    let entry = entries
        .iter()
        .find(|entry| {
            entry.get("sessionId").and_then(JsValue::as_string) == Some(child)
                && entry
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|kind| kind.equals_str("subagent_descriptor"))
        })
        .ok_or_else(|| {
            error(concat(&[
                &"Continuable subagent is not resumable: ".into(),
                child,
            ]))
        })?;
    let descriptor = entry
        .get("descriptor")
        .and_then(JsValue::as_object)
        .ok_or_else(|| error("Persisted subagent descriptor must be an object."))?;
    if descriptor.get("version") != Some(&JsValue::Number(2.0))
        || descriptor.get("mode") != Some(&"continuable".into())
    {
        return Err(error("Persisted subagent is not continuable."));
    }
    for (name, _) in descriptor.entries() {
        if ![
            "version",
            "mode",
            "provider",
            "definitionId",
            "parentSessionId",
            "label",
            "agentProvider",
            "agentModel",
        ]
        .iter()
        .any(|field| name.equals_str(field))
        {
            return Err(error("Persisted subagent descriptor has an unknown field."));
        }
    }
    for field in [
        "provider",
        "definitionId",
        "parentSessionId",
        "label",
        "agentProvider",
        "agentModel",
    ] {
        require_nonempty(
            &string(descriptor, field).ok_or_else(|| {
                error(format!(
                    "Subagent descriptor {field} must be a non-empty string."
                ))
            })?,
            field,
        )?;
    }
    if string(descriptor, "provider").as_ref() != Some(&"pilotdeck-native".into()) {
        return Err(error("Persisted subagent provider is unsupported."));
    }
    Ok(descriptor.clone())
}

struct ContinuationTool {
    fields: JsObject,
    followup: bool,
}
impl ContinuationTool {
    fn new(followup: bool) -> Self {
        let name = if followup { "send_message" } else { "subagent" };
        let schema = if followup {
            r#"{"type":"object","required":["subagent_id","message"],"additionalProperties":false,"properties":{"subagent_id":{"type":"string"},"message":{"type":"string"}}}"#
        } else {
            r#"{"type":"object","required":["description","prompt"],"additionalProperties":false,"properties":{"description":{"type":"string"},"prompt":{"type":"string"},"subagent_type":{"type":"string"}}}"#
        };
        let mut fields=object([
            ("name",name.into()),("kind","agent".into()),
            ("description",if followup{
                "Send a follow-up message to a continuable subagent. The call returns after durable admission.".into()
            }else{
                "Start a continuable background subagent. Available types: general-purpose, explore, plan, verify. The call returns after the initial message is durably admitted.".into()
            }),
            ("requiredRuntimeCapabilities",JsValue::Array(vec!["subagent_fork".into()])),
            ("isOpenWorld",JsValue::Bool(true)),("requiresUserInteraction",JsValue::Bool(false)),
            ("inputSchema",pilotdeck_protocol::parse_json(schema).expect("static continuation schema")),
        ]).as_object().unwrap().clone();
        if !followup {
            fields.insert(
                "aliases".into(),
                JsValue::Array(vec!["continuable_subagent".into()]),
            );
        }
        Self { fields, followup }
    }
}
impl ToolDefinition for ContinuationTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(vec!["subagent_fork".into()])
    }
}
impl ExecutableTool for ContinuationTool {
    #[cfg(unix)]
    fn enforces_native_sandbox(
        &self,
        context: &ToolRunContext,
        provider: &Arc<crate::runtime::tools::sandbox::NativeSandbox>,
    ) -> bool {
        context
            .host
            .subagent
            .as_ref()
            .and_then(|port| port.downcast_ref::<ForkApi>())
            .and_then(|api| api.turn.sandbox.as_ref())
            .is_some_and(|sandbox| Arc::ptr_eq(sandbox, provider))
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let followup = self.followup;
        Box::pin(async move {
            if context.is_aborted() {
                return Err(ToolError::aborted());
            }
            let api = context
                .host
                .subagent
                .clone()
                .ok_or_else(|| error("Native continuation service is unavailable."))?
                .downcast::<ForkApi>()
                .map_err(|_| error("Native continuation service has the wrong type."))?;
            api.continue_input(input, context, followup).await
        })
    }
}
pub(super) fn register(tools: &ToolRegistry) {
    for followup in [false, true] {
        tools
            .register(Arc::new(ContinuationTool::new(followup)))
            .expect("native continuation names are unique");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_ts_admission_output_and_first_descriptor_contract() {
        let oracle =
            pilotdeck_protocol::parse_json(include_str!("agent_continuation_oracle.json")).unwrap();
        let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
            panic!("cases")
        };
        let mut outputs = 0;
        for case in cases {
            if let Some(expected) = case.get("result") {
                let data = expected.get("data").unwrap();
                let item = QueuedTurn {
                    item_id: string(data.as_object().unwrap(), "messageId").unwrap(),
                    turn_id: string(data.as_object().unwrap(), "turnId").unwrap(),
                    input: JsValue::Null,
                    submit_options: JsObject::new(),
                };
                let output = admission_output(
                    &string(data.as_object().unwrap(), "subagentId").unwrap(),
                    &item,
                );
                assert_eq!(
                    object([
                        ("data", output.data.unwrap()),
                        ("content", JsValue::Array(output.content))
                    ]),
                    *expected
                );
                outputs += 1;
            }
            if case.get("id") == Some(&"first-descriptor-authoritative".into()) {
                let JsValue::Array(entries) = case.get("entries").unwrap() else {
                    panic!("entries")
                };
                assert_eq!(
                    &JsValue::Object(persisted_descriptor(entries, &"child".into()).unwrap()),
                    case.get("descriptor").unwrap()
                );
                assert!(persisted_descriptor(entries, &"another-child".into()).is_err());
                let mut bad = entries.clone();
                let mut descriptor = case.get("descriptor").unwrap().as_object().unwrap().clone();
                descriptor.insert("extra".into(), JsValue::Bool(true));
                bad[0] = object([
                    ("type", "subagent_descriptor".into()),
                    ("sessionId", "child".into()),
                    ("descriptor", JsValue::Object(descriptor)),
                ]);
                assert!(persisted_descriptor(&bad, &"child".into()).is_err());
            }
        }
        assert_eq!(outputs, 2);
        let tool = ContinuationTool::new(false);
        assert_eq!(
            tool.fields.get("aliases"),
            Some(&JsValue::Array(vec!["continuable_subagent".into()]))
        );
        assert_eq!(
            tool.is_read_only(&JsValue::Null).unwrap(),
            JsValue::Bool(false)
        );
    }
}
