//! Wire adaptation for the native replay and durable run registry.
use super::super::{
    coordinator::TurnEventCoordinator,
    read::{self, RunEventsInput, TurnReadError, TurnRunReader},
    replay::ActiveTurnSnapshotInput,
};
use crate::{runtime::connection::RequestError, storage::RunRef};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub async fn handle(
    coordinator: &TurnEventCoordinator,
    registry: &dyn TurnRunReader,
    method: &str,
    params: &JsValue,
) -> Result<JsValue, RequestError> {
    if matches!(params, JsValue::Null | JsValue::Undefined) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(RequestError::new(format!(
            "Cannot read properties of {kind} (reading 'sessionKey')"
        )));
    }
    if method == "active_turn_snapshot" {
        // TS Map keys are not coerced: an absent/non-string key cannot alias "".
        let session = match params.get("sessionKey") {
            Some(JsValue::String(session)) => session.clone(),
            value => {
                let mut result = JsObject::new();
                result.insert("active".into(), JsValue::Bool(false));
                result.insert(
                    "sessionKey".into(),
                    value.cloned().unwrap_or(JsValue::Undefined),
                );
                result.insert("events".into(), JsValue::Array(Vec::new()));
                return Ok(JsValue::Object(result));
            }
        };
        let mut input = ActiveTurnSnapshotInput::new(session);
        input.include_events = params.get("includeEvents") != Some(&JsValue::Bool(false));
        return Ok(read::active_turn_snapshot(coordinator, &input).to_json());
    }
    if method == "run_events" {
        read::validate_run_pagination(params.get("afterSeq"), params.get("limit"))
            .map_err(request_error)?;
    }
    // RunRegistry.ts:70 uses template literals rather than accepting only
    // strings. Preserve missing/null/number/object identities and conversion
    // errors instead of silently aliasing the empty-session record.
    let project_key = params
        .get("projectKey")
        .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
        .map(coerce_key)
        .transpose()?;
    let session = coerce_key(params.get("sessionKey").unwrap_or(&JsValue::Undefined))?;
    let run = RunRef {
        session_key: session,
        project_key,
        run_id: coerce_key(params.get("runId").unwrap_or(&JsValue::Undefined))?,
    };
    match method {
        "run_get" => read::run_get(Some(registry), &run)
            .await
            .map(|record| record.unwrap_or(JsValue::Undefined)),
        "run_reattach" => read::run_reattach(Some(registry), &run)
            .await
            .map(|record| record.unwrap_or(JsValue::Undefined)),
        "run_events" => {
            read::run_events(
                Some(registry),
                &RunEventsInput {
                    run,
                    after_seq: params.get("afterSeq").cloned(),
                    limit: params.get("limit").cloned(),
                },
            )
            .await
        }
        _ => unreachable!("only read methods enter this adapter"),
    }
    .map_err(request_error)
}

fn coerce_key(value: &JsValue) -> Result<JsString, RequestError> {
    crate::js_value::to_string_units_checked(value)
        .map(JsString::from_utf16)
        .map_err(|error| RequestError::new(error.message()))
}

pub(crate) fn request_error(error: TurnReadError) -> RequestError {
    RequestError {
        code: error
            .code
            .unwrap_or_else(|| "gateway_request_failed".into()),
        message: error.message,
        details: None,
        validation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::turn::publish::FileRegistry,
        storage::{FileRunRegistry, RunAcceptInput},
    };
    use pilotdeck_protocol::parse_json;

    #[tokio::test]
    async fn snapshot_preserves_non_string_session_keys_without_aliasing_empty_session() {
        let scratch = crate::scratch::Scratch::new("snapshot-session-identity");
        let registry =
            FileRegistry::new(FileRunRegistry::open(scratch.path().join("runs.json")).unwrap())
                .unwrap();
        let coordinator = TurnEventCoordinator::default();
        let _registration = coordinator.start(&"".into(), &"empty-session-run".into(), |_| {});
        coordinator.record(
            &"".into(),
            &parse_json(r#"{"type":"assistant_text_delta","text":"empty session only"}"#).unwrap(),
        );
        for input in [
            r#"{}"#,
            r#"{"sessionKey":null}"#,
            r#"{"sessionKey":false}"#,
            r#"{"sessionKey":7}"#,
            r#"{"sessionKey":[]}"#,
            r#"{"sessionKey":{}}"#,
        ] {
            let params = parse_json(input).unwrap();
            let result = handle(&coordinator, &registry, "active_turn_snapshot", &params)
                .await
                .unwrap();
            let mut expected = pilotdeck_protocol::JsObject::new();
            expected.insert("active".into(), JsValue::Bool(false));
            expected.insert(
                "sessionKey".into(),
                params
                    .get("sessionKey")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
            expected.insert("events".into(), JsValue::Array(Vec::new()));
            assert_eq!(result, JsValue::Object(expected), "{input}");
        }
        let empty = handle(
            &coordinator,
            &registry,
            "active_turn_snapshot",
            &parse_json(r#"{"sessionKey":""}"#).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(empty.get("active"), Some(&JsValue::Bool(true)));
        assert_eq!(empty.get("runId"), Some(&"empty-session-run".into()));
    }

    #[tokio::test]
    async fn snapshot_replays_every_published_event_at_each_causal_stage() {
        let scratch = crate::scratch::Scratch::new("snapshot-event-stages");
        let registry =
            FileRegistry::new(FileRunRegistry::open(scratch.path().join("runs.json")).unwrap())
                .unwrap();
        let coordinator = TurnEventCoordinator::default();
        let delivered = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = delivered.clone();
        let _registration = coordinator.start(&"session".into(), &"run".into(), move |event| {
            output.lock().unwrap().push(event.clone());
        });
        let params = parse_json(r#"{"sessionKey":"session","includeEvents":true}"#).unwrap();
        for event in [
            r#"{"type":"turn_started"}"#,
            r#"{"type":"input_accepted"}"#,
            r#"{"type":"model_request_started","provider":"provider","model":"model"}"#,
            r#"{"type":"assistant_text_delta","text":"first fragment"}"#,
            r#"{"type":"assistant_text_delta","text":"second fragment"}"#,
            r#"{"type":"turn_completed","finishReason":"completed"}"#,
        ] {
            assert!(coordinator.emit(&"session".into(), &parse_json(event).unwrap()));
            let snapshot = handle(&coordinator, &registry, "active_turn_snapshot", &params)
                .await
                .unwrap();
            assert_eq!(
                snapshot.get("events"),
                Some(&JsValue::Array(delivered.lock().unwrap().clone()))
            );
        }
        coordinator.retain_terminal(&"session".into(), &"run".into());
        let terminal = handle(&coordinator, &registry, "active_turn_snapshot", &params)
            .await
            .unwrap();
        assert_eq!(terminal.get("active"), Some(&JsValue::Bool(false)));
        assert_eq!(terminal.get("terminal"), Some(&JsValue::Bool(true)));
        assert_eq!(
            terminal.get("events"),
            Some(&JsValue::Array(delivered.lock().unwrap().clone()))
        );
        let hidden = handle(
            &coordinator,
            &registry,
            "active_turn_snapshot",
            &parse_json(r#"{"sessionKey":"session","includeEvents":false}"#).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(hidden.get("events"), Some(&JsValue::Array(Vec::new())));
    }

    #[tokio::test]
    async fn durable_reads_keep_errors_pagination_and_missing_result() {
        let scratch = crate::scratch::Scratch::new("turn-read-route");
        let store = FileRunRegistry::open(scratch.path().join("runs.json")).unwrap();
        let run = RunRef {
            project_key: None,
            session_key: "s".into(),
            run_id: "r".into(),
        };
        store
            .accept(&RunAcceptInput {
                run,
                request_material: "request".into(),
            })
            .unwrap();
        let registry = FileRegistry::new(store).unwrap();
        let coordinator = TurnEventCoordinator::default();
        for (params, expected) in [
            (
                r#"{"sessionKey":"s","runId":"r","afterSeq":"0"}"#,
                "INVALID_RUN_CURSOR",
            ),
            (
                r#"{"sessionKey":"s","runId":"r","limit":0}"#,
                "INVALID_RUN_LIMIT",
            ),
            (r#"{"sessionKey":"s","runId":"missing"}"#, "RUN_NOT_FOUND"),
        ] {
            let error = handle(
                &coordinator,
                &registry,
                "run_events",
                &parse_json(params).unwrap(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, expected);
        }
        let params = parse_json(r#"{"sessionKey":"s","runId":"r"}"#).unwrap();
        let record = handle(&coordinator, &registry, "run_get", &params)
            .await
            .unwrap();
        assert_eq!(record.get("runId"), Some(&JsValue::from("r")));
        assert_eq!(
            record,
            handle(&coordinator, &registry, "run_reattach", &params)
                .await
                .unwrap()
        );
        let params = parse_json(r#"{"sessionKey":"s","runId":"missing"}"#).unwrap();
        assert!(
            handle(&coordinator, &registry, "run_get", &params)
                .await
                .unwrap()
                .is_undefined()
        );
    }

    #[tokio::test]
    async fn durable_run_routes_coerce_js_identity_without_aliasing_empty_strings() {
        let scratch = crate::scratch::Scratch::new("run-js-identity");
        let store = FileRunRegistry::open(scratch.path().join("runs.json")).unwrap();
        for (project, session, run) in [
            (None, "", ""),
            (None, "undefined", "undefined"),
            (Some("false"), "null", "7"),
            (Some("[object Object]"), "a,,b", "true"),
            (None, "", "array-empty"),
        ] {
            store
                .accept(&RunAcceptInput {
                    run: RunRef {
                        project_key: project.map(Into::into),
                        session_key: session.into(),
                        run_id: run.into(),
                    },
                    request_material: session.into(),
                })
                .unwrap();
        }
        let registry = FileRegistry::new(store).unwrap();
        let coordinator = TurnEventCoordinator::default();
        for (raw, session, run) in [
            (r#"{}"#, "undefined", "undefined"),
            (
                r#"{"projectKey":false,"sessionKey":null,"runId":7}"#,
                "null",
                "7",
            ),
            (
                r#"{"projectKey":{},"sessionKey":["a",null,"b"],"runId":true}"#,
                "a,,b",
                "true",
            ),
            (
                r#"{"projectKey":null,"sessionKey":[],"runId":"array-empty"}"#,
                "",
                "array-empty",
            ),
        ] {
            let params = parse_json(raw).unwrap();
            for method in ["run_get", "run_reattach"] {
                let record = handle(&coordinator, &registry, method, &params)
                    .await
                    .unwrap();
                assert_eq!(
                    record.get("sessionKey"),
                    Some(&JsValue::from(session)),
                    "{raw}"
                );
                assert_eq!(record.get("runId"), Some(&JsValue::from(run)), "{raw}");
            }
            let events = handle(&coordinator, &registry, "run_events", &params)
                .await
                .unwrap();
            assert_eq!(events.get("events"), Some(&JsValue::Array(vec![])));
        }
        let bad_key =
            parse_json(r#"{"projectKey":{"toString":null},"afterSeq":"bad","limit":0}"#).unwrap();
        assert_eq!(
            handle(&coordinator, &registry, "run_events", &bad_key)
                .await
                .unwrap_err()
                .code,
            "INVALID_RUN_CURSOR"
        );
        let bad_key = parse_json(r#"{"projectKey":{"toString":null},"limit":0}"#).unwrap();
        assert_eq!(
            handle(&coordinator, &registry, "run_events", &bad_key)
                .await
                .unwrap_err()
                .code,
            "INVALID_RUN_LIMIT"
        );
        let bad_key = parse_json(r#"{"projectKey":{"toString":null}}"#).unwrap();
        assert_eq!(
            handle(&coordinator, &registry, "run_get", &bad_key)
                .await
                .unwrap_err()
                .message,
            JsString::from("Cannot convert object to primitive value")
        );
    }
}
