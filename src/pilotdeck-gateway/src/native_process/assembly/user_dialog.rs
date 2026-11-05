use crate::user_dialog::GatewayUserDialogBus;
use std::sync::{Arc, Mutex};

pub(super) fn build_user_dialog_bus() -> Arc<Mutex<GatewayUserDialogBus>> {
    // #1445 (phased follow-up): this bus is constructed without `on_change`,
    // so `is_connected()` (bus wired iff on_change is supplied) is the only
    // capability signal today and the RPC handlers' null tri-state /
    // lease-trim semantics (methods/user_dialog.rs) rely on that wiring, not on
    // a real change observer. Production on_change wiring and the
    // capability-criterion rework land in #1445; do not treat the current
    // gate as the final semantics.
    let user_dialog_bus = Arc::new(std::sync::Mutex::new(
        crate::user_dialog::GatewayUserDialogBus::new(
            crate::user_dialog::DialogRuntime::default(),
            None,
        ),
    ));
    let _ = &user_dialog_bus;
    user_dialog_bus
}
