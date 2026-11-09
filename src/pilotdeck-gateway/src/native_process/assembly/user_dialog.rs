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

/// Session-scoped durable dialog state; every turn supplies its current emitter.
#[derive(Clone)]
pub(crate) struct NativeDialogConfig {
    pub port: crate::runtime::tools::builtin::user_dialog::NativeUserDialogPort,
    pub bus: Arc<Mutex<GatewayUserDialogBus>>,
    pub journal: Option<Arc<dyn crate::user_dialog::UserDialogJournal>>,
}
pub(crate) async fn prepare_dialogs(
    home: std::path::PathBuf,
    project: std::path::PathBuf,
    session: pilotdeck_protocol::JsString,
    ephemeral: bool,
    bus: Arc<Mutex<GatewayUserDialogBus>>,
) -> Result<NativeDialogConfig, crate::runtime::project::RuntimeError> {
    let session_key = session.clone();
    let journal = if ephemeral {
        None
    } else {
        let journal = crate::runtime::always_on::storage::run(move || {
            let directory = super::super::methods::session_dir::bounded_chat_dir(&home, &project)
                .map_err(|error| {
                std::io::Error::other(String::from_utf16_lossy(error.message.units()))
            })?;
            let path = directory.join(format!(
                "{}.jsonl.dialogs.json",
                crate::storage::sanitize_session_id(&session)
            ));
            let journal =
                crate::user_dialog::GatewayUserDialogJournal::new(path, Default::default());
            #[cfg(unix)]
            let journal = journal
                .with_native_home(&home)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            Ok(journal)
        })
        .await
        .map_err(|error| crate::runtime::project::RuntimeError::new(error.to_string()))?;
        Some(Arc::new(journal) as Arc<dyn crate::user_dialog::UserDialogJournal>)
    };
    let mut options = crate::user_dialog::ChannelOptions::new(
        session_key,
        bus.clone(),
        Arc::new(|_| {
            Err(crate::user_dialog::DialogError::Invalid(
                "User-dialog turn emitter is not installed.".into(),
            ))
        }),
    );
    options.journal = journal.clone();
    let port = crate::runtime::tools::builtin::user_dialog::NativeUserDialogPort::new(options);
    Ok(NativeDialogConfig { port, bus, journal })
}
