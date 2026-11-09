//! Channel commands use the live native dispatcher, without retaining a host.
use super::requests::Requests;
use crate::{
    channel::protocol::{BoxFuture, ChannelControlPort, ChannelError},
    runtime::turn::gateway::GatewayRequests,
};
use pilotdeck_protocol::JsValue;
use std::sync::{Arc, OnceLock, Weak};

#[derive(Default)]
pub(super) struct NativeChannelControl {
    requests: OnceLock<Weak<Requests>>,
}

impl NativeChannelControl {
    pub(super) fn publish(&self, requests: &Arc<Requests>) -> bool {
        self.requests.set(Arc::downgrade(requests)).is_ok()
    }
}

impl ChannelControlPort for NativeChannelControl {
    fn call(
        &self,
        method: &'static str,
        params: JsValue,
    ) -> BoxFuture<Result<JsValue, ChannelError>> {
        let requests = self.requests.get().and_then(Weak::upgrade);
        Box::pin(async move {
            let requests = requests.ok_or_else(|| ChannelError::error("Gateway not ready"))?;
            requests
                .request(method.into(), params)
                .await
                .map_err(|error| {
                    ChannelError::error(String::from_utf16_lossy(error.message.units()))
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn channel_commands_share_dispatch_and_do_not_keep_a_retired_host() {
        let (_root, requests) = crate::native_process::management_tests::setup();
        let requests = Arc::new(requests);
        let port = NativeChannelControl::default();
        assert!(port.call("list_projects", JsValue::Null).await.is_err());
        assert!(port.publish(&requests));
        assert!(!port.publish(&requests));
        let expected = requests
            .request("list_projects".into(), JsValue::Null)
            .await
            .unwrap();
        assert_eq!(
            port.call("list_projects", JsValue::Null).await.unwrap(),
            expected
        );
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
        let weak = Arc::downgrade(&requests);
        drop(requests);
        assert!(weak.upgrade().is_none());
        assert!(port.call("list_projects", JsValue::Null).await.is_err());
    }
}
