//! Detached subagent hooks own their cancellation, projection quota and generation pin.
use super::*;
use crate::{
    hooks::protocol::HooksSettings,
    host::modules::lifecycle::LifecyclePort,
    runtime::{
        hooks::runtime::{AsyncHookRegistration, AsyncHookRegistrationHandler},
        hooks::{
            HookRuntime, HookRuntimeOptions, LifecycleDispatchInput, LifecycleDispatchResult,
            LifecycleRuntime,
        },
        project::{RuntimeLease, interaction_steer::InteractionSteer},
        turn::interaction::{SdkAsyncRegistrySlot, SdkHookState},
    },
};
fn failure(message: impl Into<JsString>) -> HostError {
    ModuleError::new(message).into()
}
use super::super::assembly::sdk_hooks::{HookProjectionBudget, NativeHookProjection};

pub(super) struct DetachedHookSource {
    settings: HooksSettings,
    options: HookRuntimeOptions,
    registry: Weak<NativeForkRegistry>,
    owner: Weak<NativeForkOwner>,
    session: JsString,
    emit: Arc<dyn Fn(JsValue) -> bool + Send + Sync>,
    steer: Option<InteractionSteer>,
    sdk: Option<(bool, SdkAsyncRegistrySlot)>,
}
struct DynamicRegistration {
    registry: Weak<NativeForkRegistry>,
    owner: Weak<NativeForkOwner>,
    session: JsString,
    include_events: bool,
    async_registry: SdkAsyncRegistrySlot,
    steer: Option<InteractionSteer>,
}
impl AsyncHookRegistrationHandler for DynamicRegistration {
    fn handle(&self, registration: AsyncHookRegistration) -> Result<(), HostError> {
        if !self.owner.upgrade().is_some_and(|owner| owner.is_open()) {
            return Err(failure("Native background hook session is closed."));
        }
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| failure("Native background hook registry is closed."))?;
        // TS registers against the session's active external run at marker time.
        // Idle is deliberately a no-op. Once captured, delivery never moves runs.
        let Some(turn) = registry.capture_async_turn(&self.session, self.steer.clone()) else {
            return Ok(());
        };
        let async_registry = self
            .async_registry
            .get()
            .and_then(Option::as_ref)
            .ok_or_else(|| failure("SDK async hook registry is unavailable."))?;
        async_registry.register_pinned(
            crate::runtime::async_hooks::AsyncHookRegistration {
                session_key: self.session.clone(),
                invocation_id: registration.invocation_id,
                hook_name: registration.hook_name,
                hook_event: registration.hook_event,
                timeout_ms: registration.timeout_ms,
                include_hook_events: self.include_events,
            },
            turn.turn,
            turn.guard,
        )
    }
}
impl ForkTurn {
    pub(in crate::native_process) fn bind_detached_hooks(
        self: &Arc<Self>,
        settings: HooksSettings,
        mut options: HookRuntimeOptions,
        sdk: Option<SdkHookState>,
        steer: Option<InteractionSteer>,
    ) -> Result<(), HostError> {
        options.on_async_registered = None;
        options.on_projection = None;
        let source = DetachedHookSource {
            settings: settings
                .into_iter()
                .filter(|(event, _)| {
                    matches!(
                        String::from_utf16_lossy(event.units()).as_str(),
                        "SubagentStart" | "SubagentStop"
                    )
                })
                .collect(),
            options,
            registry: Arc::downgrade(&self.registry),
            owner: self.owner.clone().unwrap_or_default(),
            session: self.parent.session_id.clone(),
            emit: self
                .session_emit
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(|_| false)),
            steer,
            sdk: sdk
                .filter(|sdk| sdk.active)
                .map(|sdk| (sdk.include_events, sdk.async_registry)),
        };
        self.detached_hook_source
            .set(Arc::new(source))
            .map_err(|_| failure("Native detached hook source is already bound."))
    }
}
impl DetachedHookSource {
    pub(super) fn async_registration(&self) -> Option<ParentAsyncRegistration> {
        self.sdk.as_ref().map(|(include, slot)| {
            Arc::new(DynamicRegistration {
                registry: self.registry.clone(),
                owner: self.owner.clone(),
                session: self.session.clone(),
                include_events: *include,
                async_registry: slot.clone(),
                steer: self.steer.clone(),
            }) as ParentAsyncRegistration
        })
    }

    pub(super) fn create(&self, generation: &RuntimeLease) -> Result<DetachedHooks, ToolError> {
        let lease = generation
            .fork()
            .map_err(|error| super::error(error.to_string()))?;
        let cancel = Cancellation::new();
        let mut options = self.options.clone();
        let sdk = self.sdk.as_ref().is_some_and(|(include, _)| *include);
        options.on_projection = Some(Arc::new(NativeHookProjection {
            session: self.session.clone(),
            sdk,
            emit: self.emit.clone(),
            error_emit: self.emit.clone(),
            signal: cancel.clone(),
            budget: Arc::new(HookProjectionBudget::default()),
        }));
        options.on_async_registered = self.async_registration();
        Ok(DetachedHooks(Arc::new(Owned {
            runtime: LifecycleRuntime::new(HookRuntime::with_options(
                self.settings.clone(),
                options,
            )),
            cancel,
            owner: self.owner.clone(),
            closed: Mutex::new(false),
            jobs: watch::channel(0).0,
            done: watch::channel(None).0,
            lease: Mutex::new(Some(lease)),
            executor: tokio::runtime::Handle::current(),
        })))
    }
}
#[derive(Clone)]
pub(super) struct DetachedHooks(Arc<Owned>);
struct Owned {
    runtime: LifecycleRuntime,
    cancel: Cancellation,
    owner: Weak<NativeForkOwner>,
    closed: Mutex<bool>,
    jobs: watch::Sender<usize>,
    done: watch::Sender<Option<Result<(), HostError>>>,
    lease: Mutex<Option<RuntimeLease>>,
    executor: tokio::runtime::Handle,
}
struct DispatchJob(Arc<Owned>);
impl Drop for DispatchJob {
    fn drop(&mut self) {
        self.0.jobs.send_modify(|count| *count -= 1);
    }
}
impl DetachedHooks {
    pub(super) fn cancel(&self) {
        self.0.cancel.cancel();
    }
    pub(super) fn close(&self) -> HostFuture<()> {
        let mut done = self.0.done.subscribe();
        let first = {
            let mut closed = lock(&self.0.closed);
            let first = !*closed;
            *closed = true;
            first
        };
        self.cancel();
        if first {
            let owned = self.0.clone();
            crate::runtime::cleanup_task::spawn_with_error(
                &self.0.executor,
                self.0.done.clone(),
                failure("Detached hook cleanup was dropped or panicked."),
                async move {
                    let mut jobs = owned.jobs.subscribe();
                    jobs.wait_for(|count| *count == 0)
                        .await
                        .map_err(|_| failure("Detached hook dispatch completion lost."))?;
                    let hooks = owned.runtime.dispose_checked().await;
                    let lease = lock(&owned.lease).take();
                    let released = match lease {
                        Some(mut lease) => lease.release().await,
                        None => Ok(()),
                    };
                    match (hooks, released) {
                        (Ok(()), Ok(())) => Ok(()),
                        (Err(error), Ok(())) => Err(error),
                        (Ok(()), Err(error)) => Err(failure(error.to_string())),
                        (Err(first), Err(second)) => Err(failure(format!("{first}; {second}"))),
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
                    .map_err(|_| failure("Detached hook cleanup completion lost."))?;
            }
        })
    }
}
impl LifecyclePort for DetachedHooks {
    fn dispatch(&self, mut input: LifecycleDispatchInput) -> HostFuture<LifecycleDispatchResult> {
        let closed = lock(&self.0.closed);
        if *closed || !self.0.owner.upgrade().is_some_and(|owner| owner.is_open()) {
            return Box::pin(async { Err(failure("Native detached hook session is closed.")) });
        }
        self.0.jobs.send_modify(|count| *count += 1);
        let job = DispatchJob(self.0.clone());
        let runtime = self.0.runtime.clone();
        input.signal = Some(self.0.cancel.clone());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // The worker, including its job receipt, survives a dropped caller.
        self.0.executor.spawn(async move {
            let _job = job;
            let result = runtime.dispatch(input).await;
            let _ = sender.send(result);
        });
        drop(closed);
        Box::pin(async move {
            receiver
                .await
                .map_err(|_| failure("Detached hook dispatch was dropped or panicked."))?
        })
    }
    fn dispose(&self) -> HostFuture<()> {
        self.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        hooks::protocol::{HookCommand, HookCommon, HookMatcher},
        runtime::{
            hooks::exec::{ShellFuture, ShellPort, ShellRequest, ShellResult},
            project::{ProjectRuntimeRegistry, ResourceBundle},
        },
    };
    #[derive(Default)]
    struct Marker(Mutex<Vec<JsString>>);
    impl ShellPort for Marker {
        fn execute(&self, input: ShellRequest) -> ShellFuture {
            lock(&self.0).push(input.command);
            Box::pin(async {
                Ok(ShellResult {
                    exit_code: Some(0),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    timed_out: false,
                })
            })
        }
    }
    fn settings() -> HooksSettings {
        ["SubagentStart", "SubagentStop"]
            .into_iter()
            .map(|event| {
                (
                    event.into(),
                    vec![HookMatcher {
                        hooks: vec![HookCommand::Command {
                            command: event.into(),
                            shell: None,
                            asynchronous: None,
                            async_rewake: None,
                            common: HookCommon::default(),
                        }],
                        ..Default::default()
                    }],
                )
            })
            .collect()
    }
    fn input(event: &str) -> LifecycleDispatchInput {
        LifecycleDispatchInput {
            event: event.into(),
            base_input: object([("sessionId", "session".into())])
                .as_object()
                .unwrap()
                .clone(),
            payload: None,
            match_query: Some(event.into()),
            cwd: "/fixture".into(),
            env: None,
            signal: None,
        }
    }

    #[tokio::test]
    async fn inherited_child_registration_uses_parent_lookup_and_is_revoked_by_parent_owner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let project = ProjectRuntimeRegistry::new(
            "/fixture-sdk-child-registration",
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        );
        let owner = NativeForkOwner::new(project.acquire(None).unwrap());
        let registry = NativeForkRegistry::new();
        let looked_up = Arc::new(AtomicUsize::new(0));
        let calls = looked_up.clone();
        registry.install_async_turn_lookup(Arc::new(move |session, _| {
            assert_eq!(session, &JsString::from("parent-original"));
            calls.fetch_add(1, Ordering::SeqCst);
            None
        }));
        let source = DetachedHookSource {
            settings: vec![],
            options: HookRuntimeOptions::default(),
            registry: Arc::downgrade(&registry),
            owner: Arc::downgrade(&owner),
            session: "parent-original".into(),
            emit: Arc::new(|_| false),
            steer: None,
            sdk: Some((true, Arc::new(OnceLock::new()))),
        };
        let inherited = source.async_registration().unwrap();
        drop(source);
        let registration = || AsyncHookRegistration {
            hook_name: "child-read".into(),
            hook_event: "PreToolUse".into(),
            invocation_id: "fixture".into(),
            timeout_ms: Some(1000.0),
        };
        inherited.handle(registration()).unwrap();
        assert_eq!(
            looked_up.load(Ordering::SeqCst),
            1,
            "idle parent lookup is a deliberate no-op"
        );
        owner.close_admission();
        assert!(inherited.handle(registration()).is_err());
        assert_eq!(
            looked_up.load(Ordering::SeqCst),
            1,
            "closed parent cannot register for a surviving child"
        );
        owner.release_generation().await.unwrap();
        project.dispose().await.unwrap();
    }
    #[tokio::test]
    async fn detached_hook_runtime_survives_creator_scope_and_owner_drains_generation() {
        let registry = ProjectRuntimeRegistry::new(
            "/fixture-detached-hooks",
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        );
        let owner = NativeForkOwner::new(registry.acquire(None).unwrap());
        let generation = owner.generation().unwrap();
        let marker = Arc::new(Marker::default());
        let source = DetachedHookSource {
            settings: settings(),
            options: HookRuntimeOptions {
                shell: Some(marker.clone()),
                ..Default::default()
            },
            registry: Weak::new(),
            owner: Arc::downgrade(&owner),
            session: "session".into(),
            emit: Arc::new(|_| false),
            steer: None,
            sdk: None,
        };
        let hooks = source.create(&generation).unwrap();
        owner.retain_hooks(hooks.clone()).unwrap();
        drop(source);
        hooks.dispatch(input("SubagentStart")).await.unwrap();
        // Closing the creator does not close this independently pinned runtime.
        hooks.dispatch(input("SubagentStop")).await.unwrap();
        assert_eq!(
            *lock(&marker.0),
            vec![
                JsString::from("SubagentStart"),
                JsString::from("SubagentStop")
            ]
        );
        assert_eq!(generation.runtime().lease_count(), 2);
        owner.clone().drain().await.unwrap();
        assert_eq!(generation.runtime().lease_count(), 1);
        assert!(hooks.dispatch(input("SubagentStart")).await.is_err());
        drop(generation);
        owner.release_generation().await.unwrap();
        registry.dispose().await.unwrap();
    }
}
