//! Per-turn assembly from an actual generation lease, never the marker.
//! The caller supplies trusted session services and bounded, cooperative
//! recorders. This does not implement the gateway's SubmitHost/WS assembly.
use crate::{
    hooks::protocol::HooksSettings,
    host::{
        HostError, HostFuture, HostHandler, ModuleError,
        modules::{
            CompositionSwitches, ModuleRouter, event, lifecycle,
            manifest::{PermissionSnapshot, ToolSnapshot},
        },
    },
    runtime::{
        hooks::Cancellation,
        plugins::PluginSessionContributionLease,
        project::{
            ProjectResources, Resource, RuntimeLease, StageContext,
            interaction_events::{InteractionEvents, TurnEvents},
            interaction_hooks::{InteractionHooks, TurnHooks},
            interaction_permission::{
                InteractionPermission, PermissionTurnOptions, TurnPermission,
            },
            interaction_steer::{InteractionSteer, SessionSteer, TurnSteer},
        },
        steer::MutationRecorder,
    },
};
use pilotdeck_protocol::{JsObject, JsValue, admission::ExecutionIdentity};
use std::sync::{Arc, Mutex};
use tokio::{runtime::Handle, sync::watch};

/// Session authority; none of these fields are read from the sidecar payload.
pub struct InteractionTurnInput {
    /// profile is overwritten with the leased generation's actual profile.
    pub permission: PermissionTurnOptions,
    /// Host-merged SDK/session contribution hooks, plus their owning lease.
    pub hooks: HooksSettings,
    pub contribution: Option<PluginSessionContributionLease>,
    pub base_env: Option<JsObject>,
    pub signal: Cancellation,
    /// Required when the generation publishes steer. The implementation must
    /// bound its actual I/O, including detached writes; a timeout around the
    /// returned future alone is insufficient. No volatile fallback is installed.
    pub steer_recorder: Option<Arc<dyn MutationRecorder>>,
    /// Same bounded-I/O requirement, for durable steer message application.
    pub record_message: Option<Arc<dyn Fn(JsValue) -> HostFuture<()> + Send + Sync>>,
    pub messages: Vec<JsValue>,
    pub switches: CompositionSwitches,
    /// Turn callbacks supplied by the session composition (steer drain plus
    /// durable compaction). The binder installs these after revoking any stale
    /// turn authorizations; it never reuses callbacks from a previous turn.
    pub turn_ports: crate::host::modules::turn::Ports,
}
#[derive(Clone, Default)]
pub struct InteractionPorts {
    pub events: Option<TurnEvents>,
    pub permission: Option<TurnPermission>,
    pub hooks: Option<TurnHooks>,
    pub steer: Option<TurnSteer>,
}
struct Owned {
    lease: Arc<RuntimeLease>,
    contribution: Option<Arc<PluginSessionContributionLease>>,
    session_steer: Option<SessionSteer>,
    session_finish_adopted: bool,
}

/// One execute owns this handler. Cleanup survives HostClient's disposer
/// timeout and a dropped factory/execute future; leases are released last.
pub struct InteractionTurn {
    router: Arc<ModuleRouter>,
    manifest: JsObject,
    ports: InteractionPorts,
    owned: Mutex<Option<Owned>>,
    executor: Handle,
    done: watch::Sender<Option<Result<(), HostError>>>,
    record_message: Option<Arc<dyn Fn(JsValue) -> HostFuture<()> + Send + Sync>>,
}
impl InteractionTurn {
    /// Bind the four interaction resources from this lease. The caller must
    /// freshly assemble model, budget, context and capability handlers for
    /// this identity and leased generation on every call, including their
    /// state, cancellation and execution/context/observer/plan port bindings.
    /// Generation/session services (including recorders) may be shared only
    /// through the current lease/session; their turn bindings must be fresh.
    /// A new outer router containing cloned old handlers is not sufficient.
    /// This binder cannot validate the provenance of those opaque host ports.
    pub async fn bind(
        lease: impl Into<Arc<RuntimeLease>>,
        identity: &ExecutionIdentity,
        router: ModuleRouter,
        input: InteractionTurnInput,
    ) -> Result<Self, HostError> {
        Self::bind_owned(lease, identity, router, input, false).await
    }
    pub(super) async fn bind_owned(
        lease: impl Into<Arc<RuntimeLease>>,
        identity: &ExecutionIdentity,
        mut router: ModuleRouter,
        mut input: InteractionTurnInput,
        session_owned: bool,
    ) -> Result<Self, HostError> {
        let lease = lease.into();
        let executor = Handle::try_current()
            .map_err(|_| ModuleError::new("Turn binding requires a Tokio runtime."))?;
        let context = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| ModuleError::new("Leased project resources are missing."))?
            .context();
        input.permission.profile = context.profile().interaction;
        input.permission.context.rules = input.permission.rules.snapshot();
        let turn_ports = std::mem::take(&mut input.turn_ports);
        let permission_context = input.permission.context.clone();
        let catalog = router
            .capability
            .catalog()
            .ok_or_else(|| ModuleError::new("Turn capability handler is missing."))?;
        // Only these interaction handlers are rebuilt here. Their availability
        // comes solely from this lease; all other handlers obey the fresh-turn
        // precondition above. Borrowing the catalog for permission/manifest
        // does not exempt capability's execution port from that precondition.
        router.event = Default::default();
        router.permission = Default::default();
        router.lifecycle = Default::default();
        // Replace, never inherit, both the stale steer authorizations and the
        // session-supplied callbacks for this turn.
        router.turn.ports = turn_ports;
        let mut turn = Self {
            router: Arc::new(router),
            manifest: JsObject::new(),
            ports: Default::default(),
            owned: Mutex::new(Some(Owned {
                lease,
                contribution: input.contribution.map(Arc::new),
                session_steer: None,
                session_finish_adopted: false,
            })),
            executor,
            done: watch::channel(None).0,
            record_message: input.record_message,
        };
        let router = Arc::get_mut(&mut turn.router).expect("unpublished turn router");
        if let Some(resource) =
            resource::<InteractionEvents>(&context, Resource::InteractionEvents)?
        {
            turn.ports.events = Some(resource.bind_turn(
                event::TurnIdentity::new(identity.session_id.clone(), identity.turn_id.clone()),
                identity.run_id.clone(),
                router,
            )?);
        }
        if let Some(resource) =
            resource::<InteractionPermission>(&context, Resource::InteractionPermission)?
        {
            turn.ports.permission = Some(resource.bind_turn(
                identity.session_id.clone(),
                identity.run_id.clone(),
                input.permission,
                catalog.clone(),
                router,
            )?);
        }
        if let Some(resource) = resource::<InteractionHooks>(&context, Resource::InteractionHooks)?
        {
            turn.ports.hooks = Some(resource.bind_turn(
                lifecycle::Binding {
                    session_id: identity.session_id.clone(),
                    turn_id: identity.turn_id.clone(),
                    cwd: permission_context.cwd.clone(),
                    permission_mode: Some(permission_context.mode.as_js_string().clone()),
                    base_env: input.base_env,
                    signal: Some(input.signal.clone()),
                },
                identity.run_id.clone(),
                input.hooks,
                turn.ports.permission.clone(),
                router,
            )?);
        }
        if let Some(hooks) = &turn.ports.hooks {
            // A dropped stream releases the submit slot before detached hook
            // cleanup finishes, just as with steer. Supersede the old hooks
            // identity first, then wait for its admitted hook jobs to drain
            // before this turn may dispatch anything.
            hooks.wait_ready().await?;
        }
        if let Some(resource) = resource::<InteractionSteer>(&context, Resource::InteractionSteer)?
        {
            if turn.record_message.is_none() {
                return Err(
                    ModuleError::new("Steer requires a bounded durable message recorder.").into(),
                );
            }
            let recorder = input.steer_recorder.ok_or_else(|| {
                ModuleError::new("Steer requires a host recorder with bounded I/O.")
            })?;
            let session = resource.bind_session(identity.session_id.clone(), Some(recorder))?;
            // Retain even if start_turn fails so rollback disposes the session.
            turn.owned
                .get_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .session_steer = Some(session.clone());
            // A dropped stream releases the submit slot before detached
            // disposal finishes. Supersede its identity, then wait for all
            // admitted durable writes before starting the replacement mailbox.
            session.wait_ready().await?;
            turn.ports.steer = Some(session.start_owned_turn(
                identity.turn_id.clone(),
                identity.run_id.clone(),
                input.messages,
                input.signal,
                router,
                session_owned,
            )?);
        } else {
            router.turn.authorizations = None;
        }
        let tools = catalog.list().await?;
        // No question/elicitation port is supplied by these four resources.
        input.switches.elicitation_available = None;
        turn.manifest = turn
            .router
            .turn_manifest(
                tools
                    .iter()
                    .map(|tool| ToolSnapshot::from_definition(tool.as_ref()))
                    .collect(),
                PermissionSnapshot {
                    cwd: JsValue::String(permission_context.cwd.clone()),
                    permission_mode: JsValue::String(
                        permission_context.mode.as_js_string().clone(),
                    ),
                    permission_context: permission_context.to_value().as_object().unwrap().clone(),
                    turn_can_prompt: None,
                },
                &input.switches,
            )
            .map_err(|missing| {
                ModuleError::new(format!("Turn handlers missing: {}", missing.join(", ")))
            })?;
        // #686: every predecessor binding above has now passed its cleanup
        // barrier, so their discard notices are final. Hand them to this turn's
        // event buffer; the buffer appends them after `turn_started`.
        if let Some(events) = &turn.ports.events {
            let mut notices = Vec::new();
            if let Some(permission) = &turn.ports.permission {
                notices.extend(permission.take_discard_notices());
            }
            if let Some(hooks) = &turn.ports.hooks {
                notices.extend(hooks.take_discard_notices());
            }
            if let Some(steer) = &turn.ports.steer {
                notices.extend(steer.take_discard_notices());
            }
            events.queue_notices(notices);
        }
        Ok(turn)
    }
    pub fn manifest(&self) -> &JsObject {
        &self.manifest
    }
    pub(super) fn adopt_session_finish(&self) {
        if let Some(owned) = self.owned.lock().unwrap().as_mut() {
            owned.session_finish_adopted = true;
        }
    }
    pub(super) fn contribution_hold(&self) -> Option<Arc<PluginSessionContributionLease>> {
        self.owned
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|owned| owned.contribution.clone())
    }
    pub fn ports(&self) -> InteractionPorts {
        self.ports.clone()
    }
    fn close(&self) {
        let Some(mut owned) = self.owned.lock().unwrap().take() else {
            return;
        };
        let ports = self.ports.clone();
        let router = self.router.clone();
        // Revoke steer synchronously, before any unrelated disposer can stall.
        let steer_close = ports.steer.as_ref().map(TurnSteer::close);
        let rollback_steer = (!owned.session_finish_adopted)
            .then(|| ports.steer.clone())
            .flatten();
        let session_close = owned.session_steer.as_ref().map(SessionSteer::dispose);
        crate::runtime::cleanup_task::spawn_after(
            &self.executor,
            self.done.clone(),
            || {
                if let Some(events) = &self.ports.events {
                    events.close();
                }
                if let Some(permission) = &self.ports.permission {
                    permission.close();
                }
                if let Some(hooks) = &self.ports.hooks {
                    hooks.close();
                }
            },
            async move {
                // Start every cleanup even if an earlier one rejects.
                let mut errors = Vec::new();
                let (router, hooks, steer, session) = tokio::join!(
                    router.dispose(),
                    async {
                        if let Some(hooks) = ports.hooks {
                            hooks.wait_closed().await
                        } else {
                            Ok(())
                        }
                    },
                    async {
                        if let Some(close) = steer_close {
                            close.await.map(|_| ())
                        } else {
                            Ok(())
                        }
                    },
                    async {
                        if let Some(close) = session_close {
                            close.await
                        } else {
                            Ok(())
                        }
                    }
                );
                for result in [router, hooks, steer, session] {
                    if let Err(error) = result {
                        errors.push(error.to_string());
                    }
                }
                // Before publication, failed/cancelled bind still owns finish.
                if let Some(steer) = rollback_steer {
                    steer.finish_session();
                }
                // A retired project's plugin disposer may wait for this lease.
                if let Some(contribution) = owned.contribution.take()
                    && let Ok(mut contribution) = Arc::try_unwrap(contribution)
                    && let Err(error) = contribution.release().await
                {
                    errors.push(error.to_string());
                }
                // Another owner may still be recording the terminal. Waiting
                // for it here would cycle through HostClient's disposal wait.
                if let Ok(mut lease) = Arc::try_unwrap(owned.lease)
                    && let Err(error) = lease.release().await
                {
                    errors.push(error.to_string());
                }
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(ModuleError::new(errors.join("; ")).into())
                }
            },
        );
    }
    pub fn wait_closed(&self) -> HostFuture<()> {
        let mut done = self.done.subscribe();
        Box::pin(async move {
            loop {
                if let Some(result) = done.borrow_and_update().clone() {
                    return result;
                }
                done.changed()
                    .await
                    .map_err(|_| ModuleError::new("Turn cleanup completion lost."))?;
            }
        })
    }
}
impl HostHandler for InteractionTurn {
    fn drain_events(&self) -> Result<Vec<JsObject>, HostError> {
        self.ports
            .events
            .as_ref()
            .map(TurnEvents::drain)
            .unwrap_or_else(|| Ok(Vec::new()))
    }
    fn event(&self, event: JsValue) -> HostFuture<()> {
        let result = (|| {
            let Some(payload) = event.get("payload") else {
                return Ok(None);
            };
            if payload.get("type") != Some(&"steer_applied".into()) {
                return Ok(None);
            }
            let steer = self
                .ports
                .steer
                .clone()
                .ok_or_else(|| ModuleError::new("Steer is not bound."))?;
            let record = self
                .record_message
                .clone()
                .ok_or_else(|| ModuleError::new("Steer recorder is missing."))?;
            let id = payload
                .get("itemId")
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(|| ModuleError::new("Steer itemId is missing."))?;
            let message = payload
                .get("message")
                .cloned()
                .ok_or_else(|| ModuleError::new("Steer message is missing."))?;
            Ok::<_, HostError>(Some(
                steer.persist_and_apply(id, message, move |message| record(message)),
            ))
        })();
        Box::pin(async move {
            if let Some(write) = result? {
                write.await?;
            }
            Ok(())
        })
    }
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.router.module_call(call)
    }
    fn dispose(&self) -> HostFuture<()> {
        self.close();
        self.wait_closed()
    }
}
impl Drop for InteractionTurn {
    fn drop(&mut self) {
        self.close();
    }
}
fn resource<T: Clone + 'static>(
    context: &StageContext,
    key: Resource,
) -> Result<Option<T>, HostError> {
    context
        .resource(key)
        .map(|resource| {
            resource
                .as_any()
                .downcast_ref::<T>()
                .cloned()
                .ok_or_else(|| {
                    ModuleError::new(format!("Invalid leased {} resource.", key.as_str())).into()
                })
        })
        .transpose()
}
