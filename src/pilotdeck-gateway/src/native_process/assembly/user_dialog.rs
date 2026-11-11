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
    //
    // The claim above is registered as the `user-dialog-bus-observer-coupled`
    // executable mutation (`crate::mutants::on`, feature `pdrs-mutants`;
    // `tests/fixtures/user-dialog-bus-observer/negative_controls.py`, the
    // mcp-resolver-lease precedent): with the switch selected the assembly
    // reverts to the legacy observer-coupled constructor below. The branch is
    // `#[cfg(all(test, feature = "pdrs-mutants"))]` because that constructor
    // is test-only (#1600); with the feature off the switch is a constant
    // `false` and this branch does not exist, so the production bytes are the
    // `connected` call alone.
    #[cfg(all(test, feature = "pdrs-mutants"))]
    if crate::mutants::on("user-dialog-bus-observer-coupled") {
        return Arc::new(Mutex::new(GatewayUserDialogBus::new(
            crate::user_dialog::DialogRuntime::default(),
            None,
        )));
    }
    Arc::new(Mutex::new(GatewayUserDialogBus::connected(
        crate::user_dialog::DialogRuntime::default(),
    )))
}

#[cfg(test)]
mod tests {
    use super::build_user_dialog_bus;

    /// #1445 negative control: if production reverts to the legacy
    /// `new(runtime, None)` observer-coupled constructor this test turns red,
    /// restoring the unreachable family.
    /// #2887: the claim is registered as the `user-dialog-bus-observer-coupled`
    /// executable mutation (`tests/fixtures/user-dialog-bus-observer/
    /// negative_controls.py`), which must stay red at the assertion below;
    /// this prose no longer carries the proof on its own.
    #[test]
    fn production_bus_is_connected_without_an_observer() {
        let bus = build_user_dialog_bus();
        crate::negative_assert!(
            bus.lock().unwrap_or_else(|e| e.into_inner()).is_connected(),
            "#1445 production assembly must build a connected bus"
        );
    }
}
