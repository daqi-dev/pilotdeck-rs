use super::super::requests::Requests;
use super::string;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::JsValue;

// Wire names are the WebSocket frame method names from the TS frames registry
// (frames.ts:47-48). Both methods pass through admission as ordinary session
// requests; sop_resume additionally rejects an active turn before acting.
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

fn error(code: &str, message: &str) -> RequestError {
    RequestError {
        code: code.into(),
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
            let session_key = string(&params, "sessionKey")?;

            if method == "sop_resume" {
                // Clients must not override the approval authority or subject
                // field. createLocalGateway.ts:1079 applies the same guard
                // before delegating to publicApprovals or sopControl.
                for key in ["authority", "subject"] {
                    if params.get(key).is_some_and(|v| !v.is_undefined()) {
                        return Err(error(
                            "APPROVAL_AUTHORITY_OVERRIDE",
                            "Overriding approval authority or subject is not permitted.",
                        ));
                    }
                }

                // Mirrors InProcessGateway.ts:641: "Cannot resume StaffDeck
                // SOP while a turn is active."
                if self.owner.has_active_turn(&session_key) {
                    return Err(error(
                        "SESSION_BUSY",
                        "Cannot resume StaffDeck SOP while a turn is active.",
                    ));
                }
            }

            // Native isolated process has no SOP runtime attached. Return
            // CAPABILITY_UNAVAILABLE after all entry gates have been checked.
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

    // sop_resume: authority field present → APPROVAL_AUTHORITY_OVERRIDE.
    #[tokio::test]
    async fn sop_resume_rejects_authority_override() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = crate::metadata::object([
            ("sessionKey", "s1".into()),
            ("authority", "injected".into()),
        ]);
        let result = requests.sop_request("sop_resume".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "APPROVAL_AUTHORITY_OVERRIDE",
            "#1312 sop_resume rejects authority"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    // sop_resume: subject field present → APPROVAL_AUTHORITY_OVERRIDE.
    #[tokio::test]
    async fn sop_resume_rejects_subject_override() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params =
            crate::metadata::object([("sessionKey", "s1".into()), ("subject", "injected".into())]);
        let result = requests.sop_request("sop_resume".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "APPROVAL_AUTHORITY_OVERRIDE",
            "#1312 sop_resume rejects subject"
        );
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
}
