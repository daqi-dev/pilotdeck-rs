use super::super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::JsValue;

// Wire names are the WebSocket frame method names from the TS frames registry
// (frames.ts:47-48). Both methods pass through admission as ordinary session
// requests. Production installs the native project-bound control owner.
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
            if let Some(control) = &self.sop {
                if method == "sop_status" {
                    return control.status(&params).await.map_err(super::super::sop_control::request_error);
                }
                if let Some(session) = params.get("sessionKey").and_then(JsValue::as_string)
                    && self.owner.has_active_turn(session) {
                    return Err(RequestError { code: "SESSION_BUSY".into(),
                        message: "Cannot resume StaffDeck SOP while a turn is active.".into(), details: None, validation: None });
                }
                return control.resume(&params).await.map_err(super::super::sop_control::request_error);
            }
            // Bare embeddings retain the capability-first TS gate; production
            // installs the same control owner used by the public HTTP bridge.
            Err(unavailable(if method == "sop_status" {
                "StaffDeck SOP status is unavailable."
            } else {
                "StaffDeck SOP resume is unavailable."
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::native_process::management_tests::setup;
    use crate::negative_assert_eq;

    // sop_status: sessionKey missing → CAPABILITY_UNAVAILABLE, not a validation
    // error. In TS the capability gate is the first statement of sopStatus
    // (InProcessGateway.ts:635) and the connection layer never schema-parses
    // params, so the required-field check is unreachable.
    #[tokio::test]
    async fn sop_status_missing_session_key_hits_capability_gate_first() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let result = requests.sop_request("sop_status".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1312 sop_status missing sessionKey hits capability gate first"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    // Fixed TS asks for the optional SOP capability before reading projectKey;
    // the native process retains its previously documented local error text.
    #[tokio::test]
    async fn sop_status_rejects_wrong_project() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/native-multiproject-gates/oracle.json"
        ))
        .unwrap();
        let cases = match oracle.get("cases").unwrap() {
            pilotdeck_protocol::JsValue::Array(cases) => cases,
            _ => panic!("cases"),
        };
        for (name, key) in [
            ("null-sop", pilotdeck_protocol::JsValue::Null),
            ("number-sop", pilotdeck_protocol::JsValue::Number(5.0)),
            (
                "foreign-sop",
                pilotdeck_protocol::JsValue::from("/wrong/project"),
            ),
        ] {
            let expected = cases
                .iter()
                .find(|case| {
                    case.get("id")
                        .and_then(pilotdeck_protocol::JsValue::as_string)
                        .is_some_and(|id| id.equals_str(name))
                })
                .unwrap();
            let params =
                crate::metadata::object([("sessionKey", "s1".into()), ("projectKey", key)]);
            let error = crate::runtime::turn::gateway::GatewayRequests::request(
                &requests,
                pilotdeck_protocol::JsString::from("sop_status"),
                params,
            )
            .await
            .unwrap_err();
            negative_assert_eq!(
                Some(&pilotdeck_protocol::JsValue::from(error.code.as_str())),
                expected.get("code"),
                "#1312 unconfigured SOP code matches fixed TS"
            );
            assert_eq!(
                Some(&pilotdeck_protocol::JsValue::String(error.message.clone())),
                expected.get("message"),
                "#1312 status message matches fixed TS"
            );
        }
        for (name, key) in [
            ("null-resume", pilotdeck_protocol::JsValue::Null),
            ("number-resume", pilotdeck_protocol::JsValue::Number(5.0)),
            (
                "foreign-resume",
                pilotdeck_protocol::JsValue::from("/wrong/project"),
            ),
        ] {
            let expected = cases
                .iter()
                .find(|case| {
                    case.get("id")
                        .and_then(pilotdeck_protocol::JsValue::as_string)
                        .is_some_and(|id| id.equals_str(name))
                })
                .unwrap();
            let params =
                crate::metadata::object([("sessionKey", "s1".into()), ("projectKey", key)]);
            let error = crate::runtime::turn::gateway::GatewayRequests::request(
                &requests,
                pilotdeck_protocol::JsString::from("sop_resume"),
                params,
            )
            .await
            .unwrap_err();
            negative_assert_eq!(
                Some(&pilotdeck_protocol::JsValue::from(error.code.as_str())),
                expected.get("code"),
                "#1312 resume code matches fixed TS"
            );
            assert_eq!(
                Some(&pilotdeck_protocol::JsValue::String(error.message.clone())),
                expected.get("message"),
                "#1312 resume message matches fixed TS"
            );
        }
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

    // sop_resume: every required field — sessionKey, message, requestId,
    // waitId, source — missing. TS still answers CAPABILITY_UNAVAILABLE: the
    // capability gate is the first statement of resumeSop
    // (InProcessGateway.ts:640) and nothing above it parses params, so a
    // single-field validation added for sop_resume ahead of the capability
    // return would drift from TS to gateway_request_failed.
    #[tokio::test]
    async fn sop_resume_missing_required_fields_hits_capability_gate_first() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let result = requests.sop_request("sop_resume".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1312 sop_resume missing all required fields still hits capability gate first"
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
            "StaffDeck SOP status is unavailable.",
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
