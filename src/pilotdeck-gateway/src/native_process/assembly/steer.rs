use super::{
    AsyncHookRegistry, AsyncHookTurn, JsValue, ProjectResources, ProjectRuntimeRegistry, Resource,
    SessionOwner, TokioHookClock,
};
use crate::runtime::turn::coordinator::TurnEventCoordinator;
use std::sync::Arc;

pub(super) fn build_async_hook_registry(
    build_project: &Arc<ProjectRuntimeRegistry>,
    owner: &SessionOwner,
    coordinator: &Arc<TurnEventCoordinator>,
) -> Option<Arc<AsyncHookRegistry>> {
    // Build the async-hook registry only when the project stages an
    // InteractionSteer resource (i.e. the steer pathway is wired).
    // Check via ProjectResources→StageContext since InteractionSteer does
    // not implement RuntimeResources directly.
    let async_hook_registry = build_project
        .acquire(None)
        .ok()
        .and_then(|lease| {
            lease
                .runtime()
                .resources()
                .get::<ProjectResources>()?
                .context()
                .resource(Resource::InteractionSteer)
                .map(|_| ())
        })
        .map(|_| {
            let reg_project = build_project.clone();
            let reg_owner = owner.clone();
            let reg_coordinator = coordinator.clone();
            let registry = AsyncHookRegistry::new(
                Arc::new(TokioHookClock),
                move |session_key| {
                    // Turn is live only when the admission slot holds a run ID.
                    let run_id = reg_owner.active_turn_run_id(session_key)?;
                    // Shared lookup: fetch the InteractionSteer resource
                    // and guard the run ID against the admission slot.
                    let info = crate::runtime::project::interaction_steer::active_turn_for(
                        &reg_project,
                        session_key,
                        Some(&run_id),
                    )?;
                    Some(AsyncHookTurn {
                        run_id: info.turn_id,
                        mailbox: info.mailbox,
                    })
                },
                move |session_key, event| {
                    reg_coordinator.emit(session_key, &JsValue::Object(event));
                },
            );
            Arc::new(registry)
        });
    let _ = &async_hook_registry;
    async_hook_registry
}
