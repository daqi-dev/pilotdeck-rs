//! A6.9-pr3c2: interaction capability marker for the project factory.
//!
//! This module publishes one stateless [`Resource::Interaction`] marker under
//! the execution-world family. It deliberately carries no ports and no
//! per-turn state.
//!
//! - **Consumption boundary**: the marker means only that this generation was
//!   built with the interaction capability selected by the factory builders.
//!   It is not a profile switch and not a permission bus, a hook/lifecycle
//!   runtime, an event sink or a steer mailbox, and it must not be used as
//!   proof that those ports exist or are bound. The runtime profile stays
//!   authoritative: a Disabled or Headless profile remains disabled/headless
//!   even when this marker is published, and consumers must gate permission,
//!   prompt and port decisions on the profile, never on the marker. The
//!   permission, hooks/lifecycle, event and steer ports are composed per turn
//!   by the session/turn layer in A7.2-pr6 (#589). Until that lands, consumers
//!   keep their existing disabled / fail-closed handling and must not enable
//!   interaction paths from this marker alone.
//! - **Disposal boundary**: the marker owns no OS or runtime resources, so its
//!   `FamilyResource::dispose` is a no-op. The wrapped execution-world source
//!   keeps its own dispose owner and the factory's disposal order; this
//!   wrapper only forwards staging and never replaces that owner.
//!
//! Baseline (cc7c4e6) anchors: `ProjectExecutionWorldBundle.ts:18-21,41-44`
//! exposes only `executionWorld`/`tools`, while
//! `sidecarTurnComposition.ts:85-96` binds permission/lifecycle/event from the
//! per-turn `modules` ports, independently of the execution world. Permission
//! and hooks runtimes live in `runtime/permission.rs` and
//! `runtime/hooks/runtime.rs`, the event sink in `host/modules/event.rs`, and
//! steer in `runtime/steer.rs`.
use super::*;
use std::{any::Any, sync::Arc};

// ---------------------------------------------------------------------------
// Interaction family (factory capability marker)
// ---------------------------------------------------------------------------

/// Opaque marker resource. It holds no interaction port; the type is distinct
/// from the execution world so consumers cannot mistake one for the other.
struct InteractionResource;

impl FamilyResource for InteractionResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Layers the interaction marker over an execution-world source. Wrapping
/// (rather than replacing) the source keeps the execution-world owner and
/// resource intact, so `with_interaction` composes with `with_tools_mcp` in
/// either order and preserves a custom execution-world source until a builder
/// replaces that kernel. `with_tools_mcp` always swaps in the native source and
/// keeps only this marker layering.
///
/// This is a transparent wrapper and therefore forwards
/// `layers_interaction() == true`. Any decorator placed between this wrapper
/// and the execution world must likewise forward the answer of the source it
/// delegates to; otherwise the builders treat it as an ordinary source and may
/// replace or re-wrap it (see `ResourceFamilySource::layers_interaction`).
pub struct InteractionFamilySource {
    execution_world: Arc<dyn ResourceFamilySource>,
}

impl InteractionFamilySource {
    pub fn new(execution_world: Arc<dyn ResourceFamilySource>) -> Self {
        Self { execution_world }
    }
}

impl ResourceFamilySource for InteractionFamilySource {
    fn family(&self) -> Family {
        Family::ExecutionWorld
    }

    fn layers_interaction(&self) -> bool {
        true
    }

    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        // Stage the underlying execution world first: it registers the
        // execution-world owner and resource this marker sits beside.
        self.execution_world.stage(context, sink)?;
        // Publish the distinct, port-free marker; never fabricate ports here.
        sink.register_resource(Resource::Interaction, Arc::new(InteractionResource));
        Ok(())
    }
}
