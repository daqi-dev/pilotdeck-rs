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
        .try_reserve_archive_with_project(
            &session,
            &pilotdeck_protocol::JsString::from(project.to_string_lossy().as_ref()),
            restore,
        )
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
    fn check_capture_error(cases: &JsValue, name: &str, error: RequestError) {
        let expected = cases.get(name).unwrap();
        assert_eq!(expected.get("ok"), Some(&JsValue::Bool(false)), "{name}");
        assert_eq!(
            Some(&JsValue::String(error.code.into())),
            expected.get("code"),
            "{name}"
        );
        if let Some(message) = expected.get("message") {
            assert_eq!(&JsValue::String(error.message), message, "{name}");
        }
    }
    #[tokio::test]
    async fn all_thirteen_original_capture_cases_have_native_counterparts() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-portable-history/oracle.json"
        ))
        .unwrap();
        let cases = oracle.get("cases").unwrap();
        let expected_names = [
            "unknown_project_missing_session",
            "invalid_project_empty_session",
            "cold_invalid_archive",
            "empty_archive",
            "restore",
            "export_restored",
            "conflict",
            "missing",
            "busy_invalid_archive",
            "busy_export",
            "busy_restore",
            "finished_export",
            "compact_projection",
        ];
        let actual_names: std::collections::HashSet<_> = cases
            .as_object()
            .unwrap()
            .entries()
            .iter()
            .map(|(k, _)| k.to_utf8().unwrap())
            .collect();
        assert_eq!(
            actual_names,
            expected_names.into_iter().map(str::to_owned).collect()
        );
        let project = JsValue::String(requests.options.project.to_string_lossy().as_ref().into());
        let params = |session: &str, archive: JsValue| {
            crate::metadata::object([
                ("projectKey", project.clone()),
                ("sessionKey", session.into()),
                ("archive", archive),
            ])
        };
        let unknown = JsValue::String(
            requests
                .options
                .project
                .join("unknown")
                .to_string_lossy()
                .as_ref()
                .into(),
        );
        check_capture_error(
            cases,
            "unknown_project_missing_session",
            GatewayRequests::request(
                &requests,
                "export_session_transcript".into(),
                crate::metadata::object([("projectKey", unknown)]),
            )
            .await
            .unwrap_err(),
        );
        check_capture_error(
            cases,
            "invalid_project_empty_session",
            GatewayRequests::request(
                &requests,
                "export_session_transcript".into(),
                crate::metadata::object([
                    ("projectKey", JsValue::Number(23.0)),
                    ("sessionKey", "".into()),
                ]),
            )
            .await
            .unwrap_err(),
        );
        check_capture_error(
            cases,
            "cold_invalid_archive",
            GatewayRequests::request(
                &requests,
                "restore_session_transcript".into(),
                params("sdk:cold", parse_json("{}").unwrap()),
            )
            .await
            .unwrap_err(),
        );
        let empty =
            parse_json(r#"{"schemaVersion":1,"format":"portable_text_messages","messages":[]}"#)
                .unwrap();
        check_capture_error(
            cases,
            "empty_archive",
            GatewayRequests::request(
                &requests,
                "restore_session_transcript".into(),
                params("sdk:empty", empty),
            )
            .await
            .unwrap_err(),
        );
        let archive = cases
            .get("export_restored")
            .unwrap()
            .get("value")
            .unwrap()
            .clone();
        let restored = GatewayRequests::request(
            &requests,
            "restore_session_transcript".into(),
            params("sdk:restored", archive.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            &restored,
            cases.get("restore").unwrap().get("value").unwrap()
        );
        let exported = GatewayRequests::request(
            &requests,
            "export_session_transcript".into(),
            params("sdk:restored", JsValue::Undefined),
        )
        .await
        .unwrap();
        assert_eq!(
            &exported,
            cases.get("export_restored").unwrap().get("value").unwrap()
        );
        check_capture_error(
            cases,
            "conflict",
            GatewayRequests::request(
                &requests,
                "restore_session_transcript".into(),
                params("sdk:restored", archive.clone()),
            )
            .await
            .unwrap_err(),
        );
        check_capture_error(
            cases,
            "missing",
            GatewayRequests::request(
                &requests,
                "export_session_transcript".into(),
                params("sdk:missing", JsValue::Undefined),
            )
            .await
            .unwrap_err(),
        );
        let key: JsString = "sdk:busy".into();
        let run: JsString = "wire-oracle-run".into();
        let active = requests.owner.begin_turn(&key, &run).unwrap();
        for (name, method, value) in [
            (
                "busy_invalid_archive",
                "restore_session_transcript",
                parse_json("{}").unwrap(),
            ),
            (
                "busy_export",
                "export_session_transcript",
                JsValue::Undefined,
            ),
            ("busy_restore", "restore_session_transcript", archive),
        ] {
            check_capture_error(
                cases,
                name,
                GatewayRequests::request(&requests, method.into(), params("sdk:busy", value))
                    .await
                    .unwrap_err(),
            );
        }
        requests.owner.end_turn(&key, Some(&run), Some(&active));
        // This wire unit checks post-admission archive semantics. Actual model
        // completion/durable resume remains covered by the native-resume probe.
        let finished = cases
            .get("finished_export")
            .unwrap()
            .get("value")
            .unwrap()
            .clone();
        GatewayRequests::request(
            &requests,
            "restore_session_transcript".into(),
            params("sdk:busy", finished.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            GatewayRequests::request(
                &requests,
                "export_session_transcript".into(),
                params("sdk:busy", JsValue::Undefined)
            )
            .await
            .unwrap(),
            finished
        );
        let compact=parse_json(r#"[{"type":"accepted_input","turnId":"t0","sequence":1,"messages":[{"role":"user","content":[{"type":"text","text":"old"}]}]},{"type":"control_boundary","turnId":"t1","sequence":2,"boundary":{"kind":"compact","subtype":"compact_boundary","snapshot":{"version":1,"messages":[{"role":"user","content":[{"type":"text","text":"compact"}]}]}}},{"type":"accepted_input","turnId":"t2","sequence":3,"messages":[{"role":"user","content":[{"type":"text","text":"new"}]}]}]"#).unwrap();
        let JsValue::Array(ref entries) = compact else {
            unreachable!()
        };
        let projected = crate::runtime::session::project_transcript(&entries).unwrap();
        assert_eq!(
            &JsValue::Array(projected.messages),
            cases
                .get("compact_projection")
                .unwrap()
                .get("messages")
                .unwrap()
        );
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}
