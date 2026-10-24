//! A6.9-pr3c2: Interaction and hooks resource families for factory.
//!
//! This module registers the interaction resources (permission, hooks, event,
//! steer) at the factory level, mirroring pr3c1's ExecutionWorldFamilySource.
//! Actual per-turn port binding (GatewayInteractionCoordinator, HookRuntime,
//! EventSink, SteerMailbox) belongs to the session/turn layer (A7.2-pr6).
//!
//! TS baseline references:
//! - ProjectExecutionWorldBundle.ts (execution world and sandbox)
//! - sidecarTurnComposition.ts (independent permission/lifecycle/event ports)
//! - Permission runtime: runtime/permission.rs
//! - Hooks: runtime/hooks/runtime.rs (HookRuntime, LifecycleRuntime)
//! - Event: host/modules/event.rs (EventSink trait)
//! - Steer: runtime/steer.rs (SteerMailbox, MutationRecorder)
use super::*;
use std::{any::Any, sync::Arc};

// ---------------------------------------------------------------------------
// Interaction family (permission, hooks, event, steer placeholder)
// ---------------------------------------------------------------------------

/// Wraps interaction runtime capabilities as a `FamilyResource`. This is a
/// minimal registration to satisfy the factory's resource inventory; actual
/// turn-scoped instances (GatewayInteractionCoordinator, permission bus, etc.)
/// are constructed by the turn composition layer.
struct InteractionResource;

impl FamilyResource for InteractionResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Production interaction family source. Registers the interaction capability
/// (permission, hooks, event, steer) so ExecutionWorld family staging completes.
/// Per-turn handler binding happens in the session/turn layer (A7.2-pr6).
pub struct InteractionFamilySource;

impl ResourceFamilySource for InteractionFamilySource {
    fn family(&self) -> Family {
        Family::ExecutionWorld
    }

    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        // with_interaction replaces the execution-world source, so preserve
        // its resource and dispose owner before publishing the separate marker.
        super::tools_mcp::ExecutionWorldFamilySource.stage(context, sink)?;
        sink.register_resource(Resource::Interaction, Arc::new(InteractionResource));
        Ok(())
    }
}
