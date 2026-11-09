use super::super::requests::Requests;
use crate::runtime::{
    connection::{Call, RequestError},
    replacement::ReplacementError,
    turn::read::{self, SessionHistoryReader},
};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &[
    "export_session_transcript",
    "restore_session_transcript",
    "read_session_messages",
    "fork_session",
    "replace_last_turn",
    "finalize_last_turn_replacement",
    "read_subagent_messages",
];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::history_request);

fn replacement_error(error: ReplacementError) -> RequestError {
    RequestError {
        code: error.code.unwrap_or("gateway_request_failed").into(),
        message: error.message.into(),
        details: error.details.map(|d| Box::new(*d)),
        validation: None,
    }
}

async fn portable_request(
    requests: &Requests,
    method: &str,
    params: &JsValue,
) -> Result<JsValue, RequestError> {
    let selected = match params.get("projectKey") {
        None | Some(JsValue::Null | JsValue::Undefined) => JsValue::String(
            pilotdeck_protocol::JsString::from(requests.options.project.to_string_lossy().as_ref()),
        ),
        Some(value) => value.clone(),
    };
    let home = requests.options.home.clone();
    let project_home = home.clone();
    let project = tokio::task::spawn_blocking(move || {
        super::super::project_scope::registered(&project_home, &selected)
    })
    .await
    .map_err(|e| RequestError::new(e.to_string()))??;
    let session = super::require_text(
        params,
        "sessionKey",
        "INVALID_SESSION_KEY",
        "sessionKey is required.",
    )?;
    let restore = method == "restore_session_transcript";
    let guard = requests
        .owner
        .try_reserve_archive(&session, restore)
        .map_err(|e| RequestError::new(e.message))?
        .ok_or_else(|| RequestError {
            code: "SESSION_BUSY".into(),
            message: if restore {
                "Cannot restore over an active Gateway session."
            } else {
                "Cannot export a transcript while its session has an active turn."
            }
            .into(),
            details: None,
            validation: None,
        })?;
    let files = crate::runtime::history::HistoryFiles { home, project };
    if restore {
        crate::runtime::history::restore_guarded(
            files,
            session,
            params.get("archive").cloned().unwrap_or(JsValue::Undefined),
            guard,
        )
        .await
    } else {
        crate::runtime::history::export_guarded(files, session, guard).await
    }
}

impl Requests {
    pub(in crate::native_process) fn history_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        let replacement = self.replacement.clone();
        let coordinator = self.coordinator.clone();
        let options = self.options.clone();
        let project_runtime = self.project.clone();
        Box::pin(async move {
            match method.as_str() {
                "export_session_transcript" | "restore_session_transcript" => {
                    portable_request(self, &method, &params).await
                }
                "read_session_messages" => {
                    let history = crate::runtime::history::NativeHistoryReader {
                        files: crate::runtime::history::HistoryFiles {
                            home: options.home.clone(),
                            project: options.project.clone(),
                        },
                        project_runtime,
                    };
                    let project = pilotdeck_protocol::JsString::from(
                        options.project.to_string_lossy().as_ref(),
                    );
                    read::read_session_messages_for_project(
                        &coordinator,
                        Some(&history as &dyn SessionHistoryReader),
                        &params,
                        &project,
                    )
                    .await
                    .map_err(|error| RequestError {
                        code: error
                            .code
                            .unwrap_or_else(|| "gateway_request_failed".into()),
                        message: error.message,
                        details: None,
                        validation: None,
                    })
                }
                "read_subagent_messages" => crate::runtime::history::read_subagent_messages(
                    crate::runtime::history::HistoryFiles {
                        home: options.home.clone(),
                        project: options.project.clone(),
                    },
                    params,
                )
                .await
                .map_err(RequestError::new),
                "fork_session" => replacement
                    .fork_session(params)
                    .await
                    .map_err(replacement_error),
                "replace_last_turn" => replacement
                    .replace_last_turn(params)
                    .await
                    .map_err(replacement_error),
                "finalize_last_turn_replacement" => replacement
                    .finalize_last_turn_replacement(params)
                    .await
                    .map_err(replacement_error),
                _ => Err(RequestError::new("Unknown history method.")),
            }
        })
    }
}

#[cfg(test)]
mod portable_wire_tests {
    use super::*;
    use crate::runtime::turn::gateway::GatewayRequests;
    use pilotdeck_protocol::{JsString, parse_json};

    #[tokio::test]
    async fn archive_wire_roundtrip_conflict_and_busy_match_ts_oracle() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-portable-history/oracle.json"
        ))
        .unwrap();
        let cases = oracle.get("cases").unwrap();
        let archive = cases
            .get("export_restored")
            .and_then(|v| v.get("value"))
            .unwrap()
            .clone();
        let session = JsString::from("portable-wire");
        let project = JsValue::String(requests.options.project.to_string_lossy().as_ref().into());
        let params = |archive: JsValue| {
            crate::metadata::object([
                ("projectKey", project.clone()),
                ("sessionKey", JsValue::String(session.clone())),
                ("archive", archive),
            ])
        };
        let restored = GatewayRequests::request(
            &requests,
            "restore_session_transcript".into(),
            params(archive.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            restored.get("importedMessages"),
            Some(&JsValue::Number(2.0))
        );
        let exported = GatewayRequests::request(
            &requests,
            "export_session_transcript".into(),
            params(JsValue::Undefined),
        )
        .await
        .unwrap();
        assert_eq!(exported, archive);
        let conflict = GatewayRequests::request(
            &requests,
            "restore_session_transcript".into(),
            params(archive),
        )
        .await
        .unwrap_err();
        assert_eq!(
            conflict.code,
            cases
                .get("conflict")
                .and_then(|v| v.get("code"))
                .and_then(JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap()
        );
        let guard = requests
            .owner
            .try_reserve_archive(&session, false)
            .unwrap()
            .unwrap();
        let busy = GatewayRequests::request(
            &requests,
            "restore_session_transcript".into(),
            params(JsValue::Null),
        )
        .await
        .unwrap_err();
        assert_eq!(
            busy.code,
            cases
                .get("busy_invalid_archive")
                .and_then(|v| v.get("code"))
                .and_then(JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap()
        );
        drop(guard);
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}
