//! Shared native dialog ownership; channel and storage behavior stay in user_dialog.
pub use crate::user_dialog::{
    DialogPersistence, DialogSession, DialogSessionResolver, DialogStore, GatewayUserDialogs,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
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
