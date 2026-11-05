use super::super::requests::Requests;
use super::string;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::JsValue;

// Wire names are the WebSocket frame method names from the TS frames registry
// (frames.ts:47-48). Both methods pass through admission as ordinary session
// requests; the isolated native process never attaches an SOP runtime.
pub(super) const METHODS: &[&str] = &["sop_status", "sop_resume"];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::sop_request);

fn unavailable(message: &str) -> RequestError {
    RequestError {
        code: "CAPABILITY_UNAVAILABLE".into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

impl Requests {
    pub(in crate::native_process) fn sop_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            // sessionKey is required for both methods (GatewaySopStatusInput).
            let _session_key = string(&params, "sessionKey")?;
            let _ = method;

            // The isolated native process never attaches an SOP runtime, so
            // both methods return CAPABILITY_UNAVAILABLE. For sop_resume the
            // TS capability gate fires before every other guard
            // (GatewayWsConnection.ts:332 and InProcessGateway.ts:640, ahead
            // of the :641 busy gate and the createLocalGateway.ts:1066-1069
            // authority/subject override guard): a request carrying an
            // override or racing an active turn still fails with
            // CAPABILITY_UNAVAILABLE, like a TS gateway whose resumeSop
            // callback is absent. The busy and override guards return
            // together with a wired SOP runtime (#1313 and follow-ups).
            Err(unavailable(
                "StaffDeck SOP is not configured on this isolated native process.",
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::native_process::management_tests::setup;
    use crate::negative_assert_eq;

    // sop_status: sessionKey missing → validation error, not CAPABILITY_UNAVAILABLE.
    #[tokio::test]
    async fn sop_status_requires_session_key() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let result = requests.sop_request("sop_status".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "gateway_request_failed",
            "#1312 sop_status missing sessionKey"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    // sop_status: valid sessionKey + wrong projectKey → shared entry gate in
    // Requests::request rejects before sop_request runs, code is gateway_request_failed.
    // The per-method guard that used to live in sop_request was dead code because
    // requests.rs:88-97 always intercepts first with a different error code.
    #[tokio::test]
    async fn sop_status_rejects_wrong_project() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = crate::metadata::object([
            ("sessionKey", "s1".into()),
            ("projectKey", "/wrong/project".into()),
        ]);
        let result = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            pilotdeck_protocol::JsString::from("sop_status"),
            params,
        )
        .await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "gateway_request_failed",
            "#1312 sop_status wrong project rejected by shared entry gate"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    // sop_status: valid sessionKey, no SOP runtime → CAPABILITY_UNAVAILABLE.
    #[tokio::test]
    async fn sop_status_unavailable_without_runtime() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = crate::metadata::object([("sessionKey", "s1".into())]);
        let result = requests.sop_request("sop_status".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1312 sop_status no runtime"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    // sop_resume: authority/subject override fields → the TS capability
    // gate fires before the createLocalGateway.ts:1066-1069 override guard
    // (GatewayWsConnection.ts:332, InProcessGateway.ts:640), so the isolated
    // process answers CAPABILITY_UNAVAILABLE, not APPROVAL_AUTHORITY_OVERRIDE.
    #[tokio::test]
    async fn sop_resume_override_input_hits_capability_gate_first() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        for key in ["authority", "subject"] {
            let params = crate::metadata::object([
                ("sessionKey", "s1".into()),
                (key, "injected".into()),
                ("requestId", "r1".into()),
                ("waitId", "w1".into()),
                ("source", "human".into()),
                ("message", "approved".into()),
            ]);
            let result = requests.sop_request("sop_resume".into(), params).await;
            negative_assert_eq!(
                result.unwrap_err().code.as_str(),
                "CAPABILITY_UNAVAILABLE",
                "#1312 sop_resume {key} override still hits the capability gate first"
            );
        }
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    // sop_resume: an active turn plus an authority override — the TS
    // capability gate still wins over both the busy gate
    // (InProcessGateway.ts:641) and the override guard. Mutation: moving the
    // busy or override guard ahead of the capability return flips the code.
    #[tokio::test]
    async fn sop_resume_active_turn_and_override_hit_capability_gate_first() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let session_key: pilotdeck_protocol::JsString = "s1".into();
        let run: pilotdeck_protocol::JsString = "run-1".into();
        let reservation = requests
            .owner
            .begin_turn(&session_key, &run)
            .expect("#1312 reserve active turn");
        let params = crate::metadata::object([
            ("sessionKey", "s1".into()),
            ("authority", "injected".into()),
            ("requestId", "r1".into()),
            ("waitId", "w1".into()),
            ("source", "human".into()),
            ("message", "approved".into()),
        ]);
        let result = requests.sop_request("sop_resume".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1312 sop_resume capability gate precedes busy and override"
        );
        requests
            .owner
            .end_turn(&session_key, Some(&run), Some(&reservation));
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    // sop_resume: no active turn, no runtime → CAPABILITY_UNAVAILABLE.
    #[tokio::test]
    async fn sop_resume_unavailable_without_runtime() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = crate::metadata::object([
            ("sessionKey", "s1".into()),
            ("requestId", "r1".into()),
            ("waitId", "w1".into()),
            ("source", "human".into()),
            ("message", "approved".into()),
        ]);
        let result = requests.sop_request("sop_resume".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1312 sop_resume no runtime"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    // Full wire path: a correct projectKey routes sop_status through
    // GatewayRequests::request into the sop family — not into the generic
    // metadata fallback, whose CAPABILITY_UNAVAILABLE message is
    // "Native gateway method {method} is not configured." instead.
    #[tokio::test]
    async fn sop_status_full_path_reaches_sop_family() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let project_key = requests.options.project.to_string_lossy().into_owned();
        let params = crate::metadata::object([
            ("sessionKey", "s1".into()),
            ("projectKey", project_key.as_str().into()),
        ]);
        let result = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            pilotdeck_protocol::JsString::from("sop_status"),
            params,
        )
        .await;
        let error = result.unwrap_err();
        negative_assert_eq!(
            error.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1312 sop_status full path code"
        );
        negative_assert_eq!(
            error.message.to_utf8().expect("#1312 message UTF-8"),
            "StaffDeck SOP is not configured on this isolated native process.",
            "#1312 sop_status full path reaches the sop family, not the generic fallback"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }
}
