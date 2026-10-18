//! Turn-owned host module dispatcher. Each module owns its future runtime state.
//! Capability manifest synthesis belongs to A6.9, not this routing table.
use super::{HostError, HostFuture, HostHandler, JsObject, JsValue};
pub mod budget;
pub mod capability;
pub mod context;
pub mod event;
pub mod lifecycle;
pub mod model;
pub mod permission;
pub mod turn;
pub mod wire;

#[derive(Default)]
pub struct ModuleRouter {
    pub model: model::Handler,
    pub budget: budget::Handler,
    pub capability: capability::Handler,
    pub permission: permission::Handler,
    pub context: context::Handler,
    pub turn: turn::Handler,
    pub lifecycle: lifecycle::Handler,
    pub event: event::Handler,
}
impl HostHandler for ModuleRouter {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        let Some(module) = call.get("module").and_then(JsValue::as_string) else {
            return Box::pin(async { Err(HostError::protocol("Module call target is invalid.")) });
        };
        if module.equals_str("model") {
            return self.model.call(call);
        }
        if module.equals_str("budget") {
            return self.budget.call(call);
        }
        if module.equals_str("capability") {
            return self.capability.call(call);
        }
        if module.equals_str("permission") {
            return self.permission.call(call);
        }
        if module.equals_str("context") {
            return self.context.call(call);
        }
        if module.equals_str("turn") {
            return self.turn.call(call);
        }
        if module.equals_str("lifecycle") {
            return self.lifecycle.call(call);
        }
        if module.equals_str("event") {
            return self.event.call(call);
        }
        let error = wire::named_error("Unsupported sidecar host module: ", module, "");
        Box::pin(async move { Err(error) })
    }
    fn dispose(&self) -> HostFuture<()> {
        // Construct and poll each cleanup independently, like TS allSettled:
        // one rejected cleanup cannot skip another module's resources.
        let model = self.model.dispose();
        let budget = self.budget.dispose();
        let capability = self.capability.dispose();
        let permission = self.permission.dispose();
        let context = self.context.dispose();
        let turn = self.turn.dispose();
        let lifecycle = self.lifecycle.dispose();
        let event = self.event.dispose();
        Box::pin(async move {
            let _ = tokio::join!(
                model, budget, capability, permission, context, turn, lifecycle, event
            );
            Ok(())
        })
    }
}
