use crate::user_dialog::GatewayUserDialogBus;
use std::sync::{Arc, Mutex};

pub(super) fn build_user_dialog_bus() -> Arc<Mutex<GatewayUserDialogBus>> {
    // #1445: the production bus is *live* even though no renderer change
    // observer is attached. Availability is method presence, not observer
    // wiring: TS `GatewayWsConnection.ts:481-490` gates on
    // `this.options.gateway.listUserDialogs` (always defined by
    // `InProcessGateway.ts:2710/2734/2765`), and an empty bus returns
    // `{dialogs: []}`. Coupling `is_connected()` to `on_change` made the whole
    // family unreachable in native mode.
    //
    // The observer argument stays `None`: forwarding `user_dialog_changed` is a
    // separate renderer-broadcast gap (there is no production
    // `NotificationServer`/`broadcastNotification` in the native process).
    // Tracked separately; it must not gate availability.
    Arc::new(Mutex::new(GatewayUserDialogBus::connected(
        crate::user_dialog::DialogRuntime::default(),
    )))
}

#[cfg(test)]
mod tests {
    use super::build_user_dialog_bus;

    /// #1445 negative control: if production reverts to
    /// `GatewayUserDialogBus::new(runtime, None)` (observer-coupled
    /// connectivity) this test turns red, restoring the unreachable family.
    #[test]
    fn production_bus_is_connected_without_an_observer() {
        let bus = build_user_dialog_bus();
        assert!(
            bus.lock().unwrap_or_else(|e| e.into_inner()).is_connected(),
            "#1445 production assembly must build a connected bus"
        );
    }
}
