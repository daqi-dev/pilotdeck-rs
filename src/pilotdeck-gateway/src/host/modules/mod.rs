//! Turn-owned host module dispatcher. Each module owns its future runtime state.
//! The manifest is synthesized per turn from the very handlers this router
//! dispatches to, so every advertised capability is backed by the handler
//! that will actually take the call (TS sidecarDefaultModuleDispatcher.ts:66-202
//! registration vs :394-437 advertisement).
use super::{HostError, HostFuture, HostHandler, JsObject, JsValue};
pub mod budget;
pub mod capability;
pub mod context;
pub mod event;
pub mod lifecycle;
pub mod manifest;
pub mod model;
pub mod model_port;
pub mod permission;
pub mod tool_context;
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
/// Composition switches the manifest cannot derive from bound handlers.
/// The plan/todo advertisement comes from `modules.planTodo` while the handler
/// is bound per turn by the turn composition (`sidecarTurnComposition.ts:83-87`)
/// — two independent switches by design (#570 scope note) — and elicitation
/// comes from `modules.interaction?.elicitationAvailable`, which is not a
/// host module at all.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompositionSwitches {
    /// `modules.planTodo` presence: advertise `plan_todo` on capability.
    pub plan_todo: bool,
    /// `modules.interaction?.elicitationAvailable`; None = no interaction module.
    pub elicitation_available: Option<bool>,
}
impl ModuleRouter {
    /// The module inventory the manifest advertises, derived from the
    /// per-turn bound handlers exactly like TS derives advertisement from
    /// the modules the dispatcher was constructed with (dispatcher:394-437).
    pub fn module_inventory(&self, switches: &CompositionSwitches) -> manifest::ModuleInventory {
        manifest::ModuleInventory {
            model: self.model.inventory(),
            budget: self.budget.inventory(),
            plan_todo: switches.plan_todo,
            context: self.context.inventory(),
            permission: self.permission.is_installed(),
            lifecycle: self.lifecycle.is_installed(),
            event: self.event.is_installed(),
            elicitation_available: switches.elicitation_available,
        }
    }
    /// Modules advertised without a bound handler, in TS manifest order.
    ///
    /// The item-by-item check behind "a capability without a handler must not
    /// be advertised": `model` and `capability` are TS-mandatory modules, so
    /// an unbound port has no TS-reachable counterpart and fails closed
    /// here instead of being advertised. `plan_todo` is exempt: its
    /// advertisement and its handler are independent switches (#570).
    pub fn missing_handlers(&self, inventory: &manifest::ModuleInventory) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.model.is_installed() {
            missing.push("model");
        }
        if inventory.budget.is_some() && !self.budget.is_installed() {
            missing.push("budget");
        }
        if !self.capability.is_installed() {
            missing.push("capability");
        }
        if inventory.context.is_some() && !self.context.is_installed() {
            missing.push("context");
        }
        if inventory.permission && !self.permission.is_installed() {
            missing.push("permission");
        }
        if inventory.lifecycle && !self.lifecycle.is_installed() {
            missing.push("lifecycle");
        }
        if inventory.event && !self.event.is_installed() {
            missing.push("event");
        }
        missing
    }
    /// The modules this router can actually dispatch to, in TS registration
    /// order (dispatcher:66-202: model, budget, turn, capability, then the
    /// conditionally registered permission, context, lifecycle, event).
    pub fn callable_modules(&self) -> Vec<&'static str> {
        let mut modules = Vec::new();
        if self.model.is_installed() {
            modules.push("model");
        }
        if self.budget.is_installed() {
            modules.push("budget");
        }
        // TS registers the turn handler unconditionally (dispatcher:152).
        modules.push("turn");
        if self.capability.is_installed() {
            modules.push("capability");
        }
        if self.permission.is_installed() {
            modules.push("permission");
        }
        if self.context.is_installed() {
            modules.push("context");
        }
        if self.lifecycle.is_installed() {
            modules.push("lifecycle");
        }
        if self.event.is_installed() {
            modules.push("event");
        }
        modules
    }
    /// Compose the per-turn manifest from the bound handlers, or refuse it
    /// when an advertised module has no handler to take its calls.
    pub fn turn_manifest(
        &self,
        tools: Vec<manifest::ToolSnapshot>,
        permission: manifest::PermissionSnapshot,
        switches: &CompositionSwitches,
    ) -> Result<JsObject, Vec<&'static str>> {
        let inventory = self.module_inventory(switches);
        let missing = self.missing_handlers(&inventory);
        if missing.is_empty() {
            Ok(manifest::compose_manifest(&manifest::ManifestInput {
                tools,
                permission,
                modules: inventory,
                turn: self.turn.callbacks(),
            }))
        } else {
            Err(missing)
        }
    }
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
            return self.context.call(call, &self.budget, &self.model);
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
