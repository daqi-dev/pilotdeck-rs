use crate::user_dialog::GatewayUserDialogBus;
use std::sync::{Arc, Mutex};

pub(super) fn build_user_dialog_bus(
    notifications: Option<Arc<dyn crate::runtime::refresh::NotificationServer>>,
) -> Arc<Mutex<GatewayUserDialogBus>> {
    // Availability is independent of the optional renderer notification sink.
    let observer = notifications.map(|notifications| {
        Arc::new(move |change| {
            notifications.broadcast_notification("user_dialog_changed", &change);
            Ok(())
        })
            as Arc<
                dyn Fn(pilotdeck_protocol::JsValue) -> crate::user_dialog::Result<()> + Send + Sync,
            >
    });
    Arc::new(Mutex::new(GatewayUserDialogBus::connected_with_observer(
        crate::user_dialog::DialogRuntime::default(),
        observer,
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
        let bus = build_user_dialog_bus(None);
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
