use super::super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &["sop_status", "sop_resume"];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::sop_request);

impl Requests {
    pub(in crate::native_process) fn sop_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            let home = &self.options.home;
            let project = &self.options.project;
            match method.as_str() {
                "sop_status" => crate::runtime::sop::status(home, project, &params).await,
                "sop_resume" => {
                    // Hold admission across disk/auth awaits so a turn cannot
                    // start between the busy check and the state commit.
                    let _reservation = params
                        .get("sessionKey")
                        .and_then(JsValue::as_string)
                        .map(|key| {
                            self.owner
                                .begin_maintenance(key)
                                .map_err(|err| RequestError::new(err.message))?
                                .ok_or_else(|| RequestError {
                                    code: "SESSION_BUSY".into(),
                                    ..RequestError::new("Cannot resume SOP while a turn is active.")
                                })
                        })
                        .transpose()?;
                    crate::runtime::sop::resume(home, project, &params).await
                }
                _ => Err(RequestError::new("Unknown SOP method.")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::native_process::management_tests::setup;
    use crate::negative_assert_eq;

    #[tokio::test]
    async fn sop_status_missing_session_key_is_invalid() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let result = requests.sop_request("sop_status".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "APPROVAL_INPUT_INVALID",
            "#1312 sop_status missing sessionKey validates input"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

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

    #[tokio::test]
    async fn sop_status_reports_disabled_module() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = crate::metadata::object([("sessionKey", "s1".into())]);
        let result = requests.sop_request("sop_status".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "SOP_MODULE_DISABLED",
            "#1312 sop_status disabled module"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    #[tokio::test]
    async fn sop_resume_rejects_authority_override() {
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
                "APPROVAL_AUTHORITY_OVERRIDE",
                "#1312 sop_resume {key} override is rejected"
            );
        }
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    #[tokio::test]
    async fn sop_resume_busy_precedes_override() {
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
            "SESSION_BUSY",
            "#1312 sop_resume busy precedes override"
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

    #[tokio::test]
    async fn sop_resume_requires_revision_for_human() {
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
            "APPROVAL_INPUT_INVALID",
            "#1312 sop_resume disabled module"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

    #[tokio::test]
    async fn sop_resume_missing_required_fields_is_invalid() {
        let (_scratch, requests) =
            std::panic::catch_unwind(setup).expect("#1312 sop fixture setup");
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let result = requests.sop_request("sop_resume".into(), params).await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "APPROVAL_INPUT_INVALID",
            "#1312 sop_resume missing all required fields still validates input"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }

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
            "SOP_MODULE_DISABLED",
            "#1312 sop_status full path code"
        );
        negative_assert_eq!(
            error.message.to_utf8().expect("#1312 message UTF-8"),
            "The StaffDeck SOP module is not enabled for this project.",
            "#1312 sop_status full path reaches the sop family, not the generic fallback"
        );
        requests.owner.shutdown().await.expect("#1312 sop shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1312 sop project dispose");
    }
    #[tokio::test]
    async fn sop_native_status_resume_persist_and_replay() {
        use crate::runtime::turn::gateway::GatewayRequests;
        use pilotdeck_protocol::{JsValue, parse_json};
        let (_scratch, requests) = setup();
        let config = requests.options.home.join("pilotdeck.yaml");
        let mut yaml = std::fs::read_to_string(&config).unwrap();
        yaml.push_str(
            "
modules:
  sop:
    enabled: true
    provider: staffdeck
    endpoint: http://127.0.0.1:1
    definitionsPath: sops.json
    defaultSopId: sop
",
        );
        std::fs::write(config, yaml).unwrap();
        let sessions = requests.options.home.join("sop/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let state = parse_json(r#"{"schemaVersion":3,"sessionId":"s1","bundle":{"sops":[]},"state":{"status":"handoff","slots_json":{"retained":true}},"revision":2,"wait":{"id":"wait","kind":"external_task","createdAt":"2026-11-09T00:00:00.000Z"},"resumeRequests":{},"updatedAt":"2026-11-09T00:00:00.000Z"}"#).unwrap();
        let path = crate::runtime::sop::store::path(&sessions, "s1");
        std::fs::write(
            &path,
            pilotdeck_protocol::stringify_json(&state).unwrap().unwrap(),
        )
        .unwrap();
        let params = crate::metadata::object([("sessionKey", "s1".into())]);
        let status = requests.request("sop_status".into(), params).await.unwrap();
        assert_eq!(status.get("revision"), Some(&JsValue::Number(2.0)));
        let command = parse_json(r#"{"sessionKey":"s1","requestId":"resume-1","waitId":"wait","source":"external_task","message":"done","expectedRevision":2,"slotUpdates":{"result":"ok"}}"#).unwrap();
        let key = pilotdeck_protocol::JsString::from("s1");
        let reservation = requests.owner.begin_maintenance(&key).unwrap().unwrap();
        let before = std::fs::read(&path).unwrap();
        let busy = requests
            .request("sop_resume".into(), command.clone())
            .await
            .unwrap_err();
        assert_eq!(busy.code, "SESSION_BUSY");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(requests.owner.begin_turn(&key, &"blocked".into()).is_none());
        drop(reservation);
        let receipt = requests
            .request("sop_resume".into(), command.clone())
            .await
            .unwrap();
        assert_eq!(receipt.get("accepted"), Some(&JsValue::Bool(true)));
        assert_eq!(receipt.get("revision"), Some(&JsValue::Number(3.0)));
        let persisted = parse_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(persisted.get("revision"), Some(&JsValue::Number(3.0)));
        assert_eq!(
            persisted
                .get("state")
                .unwrap()
                .get("slots_json")
                .unwrap()
                .get("retained"),
            Some(&JsValue::Bool(true))
        );
        let replay = requests
            .request("sop_resume".into(), command)
            .await
            .unwrap();
        assert_eq!(replay.get("duplicate"), Some(&JsValue::Bool(true)));
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}
