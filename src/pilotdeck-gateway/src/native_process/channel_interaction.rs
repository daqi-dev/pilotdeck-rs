//! IM helper responses enter the same coordinator as Gateway WS responses.
//! The publication is weak: a channel must never retain a retired host.
use crate::channel::{
    bluebubbles::{HelperInteraction, InteractionResponder},
    im_interaction::InteractionPort,
    protocol::{BoxFuture, ChannelError},
    sms::{ElicitationResponse, PermissionDecision, PermissionDecisionOutcome},
};
use crate::runtime::interaction::{
    GatewayInteractionCoordinator, PermissionDecision as NativeDecision,
};
use pilotdeck_protocol::JsValue;
use std::sync::{Arc, OnceLock, Weak};

#[derive(Default)]
pub(super) struct NativeInteractionResponder {
    coordinator: OnceLock<Weak<GatewayInteractionCoordinator>>,
}
impl NativeInteractionResponder {
    pub(super) fn publish(&self, coordinator: &Arc<GatewayInteractionCoordinator>) -> bool {
        self.coordinator.set(Arc::downgrade(coordinator)).is_ok()
    }
    /// Each adapter owns its own IM pending-chat state, while all responses
    /// reach the one gateway coordinator and its actual pending requests.
    pub(super) fn port(self: &Arc<Self>) -> Arc<dyn InteractionPort> {
        Arc::new(HelperInteraction::new(self.clone()))
    }
    fn current(&self) -> Result<Arc<GatewayInteractionCoordinator>, ChannelError> {
        self.coordinator
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| ChannelError::error("Gateway not ready"))
    }
}
impl InteractionResponder for NativeInteractionResponder {
    fn respond_elicitation(
        &self,
        response: ElicitationResponse,
    ) -> BoxFuture<Result<(), ChannelError>> {
        let coordinator = self.current();
        Box::pin(async move {
            let coordinator = coordinator?;
            if let Some(request) = response.request_id {
                coordinator.respond_elicitation(
                    &response.session_key,
                    &request,
                    response.answer,
                    None,
                );
            }
            Ok(())
        })
    }
    fn permission_decide(
        &self,
        decision: PermissionDecision,
    ) -> BoxFuture<Result<PermissionDecisionOutcome, ChannelError>> {
        let coordinator = self.current();
        Box::pin(async move {
            let coordinator = coordinator?;
            let delivered = decision.request_id.is_some_and(|request_id| {
                coordinator.decide_permission(
                    &decision.session_key,
                    NativeDecision {
                        request_id,
                        decision: if decision.deny { "deny" } else { "allow" }.into(),
                        remember: (!decision.deny).then_some(JsValue::Bool(decision.remember)),
                        reason: decision
                            .deny
                            .then(|| "User denied permission from IM channel.".into()),
                    },
                    None,
                )
            });
            Ok(PermissionDecisionOutcome { delivered })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::interaction::PendingEntry;
    use std::sync::Mutex;
    fn pending(sink: &Arc<Mutex<Vec<JsValue>>>) -> PendingEntry {
        let sink = sink.clone();
        PendingEntry {
            request_id: "r".into(),
            tool_call_id: "tc".into(),
            tool_name: "bash".into(),
            resolve: Arc::new(move |value| sink.lock().unwrap().push(value)),
            reject: Arc::new(|_| {}),
        }
    }
    #[tokio::test]
    async fn native_responder_delivers_once_and_does_not_retain_host() {
        let coordinator = Arc::new(GatewayInteractionCoordinator::default());
        let responder = Arc::new(NativeInteractionResponder::default());
        assert!(responder.publish(&coordinator));
        assert!(!responder.publish(&coordinator));
        let sink = Arc::new(Mutex::new(Vec::new()));
        coordinator
            .permission_bus()
            .register(&"s".into(), pending(&sink), None)
            .unwrap();
        let decision = PermissionDecision {
            session_key: "s".into(),
            request_id: Some("r".into()),
            deny: true,
            remember: false,
        };
        assert!(
            responder
                .permission_decide(decision.clone())
                .await
                .unwrap()
                .delivered
        );
        assert!(
            !responder
                .permission_decide(decision)
                .await
                .unwrap()
                .delivered
        );
        assert_eq!(
            sink.lock().unwrap()[0].get("decision"),
            Some(&JsValue::from("deny"))
        );
        coordinator
            .elicitation_bus()
            .register(&"s".into(), pending(&sink), None)
            .unwrap();
        responder
            .respond_elicitation(ElicitationResponse {
                session_key: "s".into(),
                request_id: Some("r".into()),
                answer: "actual IM answer".into(),
            })
            .await
            .unwrap();
        assert_eq!(sink.lock().unwrap()[1], JsValue::from("actual IM answer"));
        let weak = Arc::downgrade(&coordinator);
        drop(coordinator);
        assert!(
            weak.upgrade().is_none(),
            "channel responder cannot retain a retired host"
        );
        assert!(
            responder
                .respond_elicitation(ElicitationResponse {
                    session_key: "s".into(),
                    request_id: None,
                    answer: JsValue::Undefined
                })
                .await
                .is_err()
        );
    }
}
