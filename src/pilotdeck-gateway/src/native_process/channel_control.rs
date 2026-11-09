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
            if method == "abort_turn" {
                // WS intercepts this control before the method dispatcher too.
                // A channel can issue cancellation from its own stream consumer
                // (watchdog/stop), so acknowledge cancellation here; its owned
                // turn must then drain or drop that stream in finally. Waiting
                // for that same consumer here would deadlock it.
                if let Some(key) = params.get("sessionKey").and_then(JsValue::as_string)
                    && params.get("runId").is_none_or(|run| {
                        matches!(run, JsValue::String(_) | JsValue::Undefined)
                    })
                {
                    requests.owner.start_gateway_abort(
                        key,
                        params.get("runId").and_then(JsValue::as_string),
                        params.get("reason").and_then(JsValue::as_string),
                    ).map_err(|error| ChannelError::error(error.to_string()))?;
                }
                return Ok(crate::metadata::object([("ok", JsValue::Bool(true))]));
            }
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
    async fn channel_abort_only_signals_the_selected_run_without_waiting_on_its_consumer() {
        use crate::metadata::object;
        let (_root, requests) = crate::native_process::management_tests::setup();
        let requests = Arc::new(requests);
        let port = NativeChannelControl::default();
        assert!(port.publish(&requests));
        let session = "channel-one".into();
        let other = "channel-two".into();
        let run = "current".into();
        let current = requests.owner.begin_completion(&session, &run);
        let unrelated = requests.owner.begin_completion(&other, &run);
        for params in [
            JsValue::Null,
            object([("sessionKey", JsValue::Number(1.0))]),
            object([("sessionKey", JsValue::String(session.clone())), ("runId", JsValue::Null)]),
            object([("sessionKey", JsValue::String(session.clone())), ("runId", "old".into())]),
        ] {
            port.call("abort_turn", params).await.unwrap();
            assert!(!current.is_cancelled());
            assert!(!unrelated.is_cancelled());
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), port.call("abort_turn", object([
            ("sessionKey", JsValue::String(session.clone())),
            ("runId", JsValue::String(run)),
            ("reason", "system:channel-stop".into()),
        ]))).await.expect("consumer must remain free to finish after cancellation").unwrap();
        assert!(current.is_cancelled());
        assert!(!unrelated.is_cancelled());
        requests.owner.complete_turn(&session, &current);
        requests.owner.complete_turn(&other, &unrelated);
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }

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
