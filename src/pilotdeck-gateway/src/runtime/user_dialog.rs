//! Shared native dialog ownership; channel and storage behavior stay in user_dialog.
use crate::{
    runtime::connection::RequestError,
    user_dialog::{
        DialogRuntime, GatewayUserDialogBus, GatewayUserDialogJournal, HttpGatewayUserDialogStore,
        LiveGatewayUserDialogStore, StoreKey,
    },
};
use pilotdeck_protocol::JsValue;
use std::sync::{Arc, Mutex};

pub type DialogSessionResolver =
    Arc<dyn Fn(&JsValue) -> Result<Option<DialogSession>, RequestError> + Send + Sync>;

pub struct DialogSession {
    pub key: StoreKey,
    pub journal: Option<Arc<GatewayUserDialogJournal>>,
}

#[derive(Clone)]
pub enum DialogStore {
    Native(Arc<dyn LiveGatewayUserDialogStore>),
    Http(Arc<HttpGatewayUserDialogStore>),
}

pub struct DialogPersistence {
    pub store: Option<DialogStore>,
    pub resolve_session: DialogSessionResolver,
}

pub struct GatewayUserDialogs {
    pub bus: Arc<Mutex<GatewayUserDialogBus>>,
    pub persistence: Option<Arc<DialogPersistence>>,
}

impl Default for GatewayUserDialogs {
    fn default() -> Self {
        Self {
            bus: Arc::new(Mutex::new(GatewayUserDialogBus::new(
                DialogRuntime::default(),
                None,
            ))),
            persistence: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_dialog::{
        ChannelOptions, DialogAnswer, GatewayUserDialogChannel, InputRequest,
    };

    #[test]
    fn shared_owner_settles_the_existing_native_channel_request() {
        let dialogs = GatewayUserDialogs::default();
        let session = "s".into();
        let channel = GatewayUserDialogChannel::new(ChannelOptions::new(
            "s",
            dialogs.bus.clone(),
            Arc::new(|_| Ok(())),
        ));
        let mut request = channel.request_input(&InputRequest {
            tool_call_id: "tool-call".into(),
            tool_name: "request_user_input".into(),
            prompt: "Which branch?".into(),
            placeholder: None,
            allow_empty: false,
            abort: None,
        });
        let id = request.request_id().clone();
        let pending = dialogs
            .bus
            .lock()
            .unwrap()
            .consume(&session, &id, "answered")
            .unwrap()
            .unwrap();
        (pending.resolve)(DialogAnswer::Answered("runtime".into()));
        assert_eq!(
            request.try_result().unwrap().unwrap(),
            DialogAnswer::Answered("runtime".into())
        );
        assert_eq!(dialogs.bus.lock().unwrap().pending_count(&session), 0);
    }
}
