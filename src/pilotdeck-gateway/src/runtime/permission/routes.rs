//! Local permission requests over the shared pending bus and grant registry.
//! Unknown methods alone return None.
use crate::metadata::object;
use crate::runtime::{connection::RequestError, interaction::GatewayInteractionCoordinator};
use pilotdeck_protocol::{JsObject, JsValue};

pub fn handle(
    method: &str,
    params: &JsValue,
    interactions: &GatewayInteractionCoordinator,
) -> Option<Result<JsValue, RequestError>> {
    match method {
        "permission_list" => Some(list(params, interactions)),
        "grant_session_permission" => Some(grant(params, interactions)),
        "permission_decide" => Some(decide(params, interactions)),
        _ => None,
    }
}

fn decide(
    params: &JsValue,
    interactions: &GatewayInteractionCoordinator,
) -> Result<JsValue, RequestError> {
    if matches!(params, JsValue::Undefined | JsValue::Null) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(RequestError::new(format!(
            "Cannot read properties of {kind} (reading 'sessionKey')"
        )));
    }
    let delivered = match (
        params.get("sessionKey").and_then(JsValue::as_string),
        params.get("requestId").and_then(JsValue::as_string),
    ) {
        (Some(owner), Some(request)) => interactions.decide_permission_wire(
            owner,
            request,
            object([
                ("requestId", JsValue::String(request.clone())),
                (
                    "decision",
                    params
                        .get("decision")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                (
                    "remember",
                    params
                        .get("remember")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                (
                    "reason",
                    params.get("reason").cloned().unwrap_or(JsValue::Undefined),
                ),
            ]),
            params.get("interactionBinding"),
        ),
        _ => false,
    };
    Ok(object([("delivered", JsValue::Bool(delivered))]))
}

fn grant(
    params: &JsValue,
    interactions: &GatewayInteractionCoordinator,
) -> Result<JsValue, RequestError> {
    if matches!(params, JsValue::Undefined | JsValue::Null) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(RequestError::new(format!(
            "Cannot read properties of {kind} (reading 'entry')"
        )));
    }
    let undefined = JsValue::Undefined;
    let entry = params.get("entry").unwrap_or(&undefined);
    let entry = match entry {
        JsValue::String(entry) => entry,
        JsValue::Undefined | JsValue::Null => {
            let kind = if entry.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            return Err(RequestError::new(format!(
                "Cannot read properties of {kind} (reading 'trim')"
            )));
        }
        _ => return Err(RequestError::new("entry.trim is not a function")),
    };
    Ok(interactions
        .grant_session_permission_wire(params.get("sessionKey").unwrap_or(&undefined), entry))
}

fn list(
    params: &JsValue,
    interactions: &GatewayInteractionCoordinator,
) -> Result<JsValue, RequestError> {
    if matches!(params, JsValue::Undefined | JsValue::Null) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(RequestError::new(format!(
            "Cannot read properties of {kind} (reading 'sessionKey')"
        )));
    }
    let undefined = JsValue::Undefined;
    let session = params.get("sessionKey").unwrap_or(&undefined);
    let session = match session {
        JsValue::String(session) if !super::trim_units(session.units()).is_empty() => session,
        JsValue::Undefined | JsValue::Null | JsValue::String(_) => {
            let mut error = RequestError::new("sessionKey is required.");
            error.code = "INVALID_PERMISSION_LIST".into();
            return Err(error);
        }
        _ => {
            return Err(RequestError::new(
                "input.sessionKey?.trim is not a function",
            ));
        }
    };
    let requests = interactions
        .permission_bus()
        .snapshot(session)
        .into_iter()
        .map(|request| {
            let mut projected = JsObject::new();
            projected.insert(
                "requestId".into(),
                request.get("requestId").cloned().unwrap(),
            );
            projected.insert(
                "toolCallId".into(),
                request
                    .get("toolCallId")
                    .cloned()
                    .unwrap_or_else(|| "".into()),
            );
            projected.insert(
                "toolName".into(),
                request
                    .get("toolName")
                    .cloned()
                    .unwrap_or_else(|| "".into()),
            );
            if let Some(payload) = request.get("payload").filter(|value| !value.is_undefined()) {
                projected.insert("payload".into(), payload.clone());
            }
            JsValue::Object(projected)
        })
        .collect();
    Ok(object([("requests", JsValue::Array(requests))]))
}

#[cfg(test)]
#[path = "decide_tests.rs"]
mod decide_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::interaction::{PendingEntry, PermissionDecision};
    use pilotdeck_protocol::{JsString, parse_json};
    use std::sync::{Arc, Mutex};

    #[test]
    fn grant_session_permission_matches_ts_entry_errors_before_session_access() {
        let interactions = GatewayInteractionCoordinator::default();
        for (params, message) in [
            (
                JsValue::Undefined,
                "Cannot read properties of undefined (reading 'entry')",
            ),
            (
                JsValue::Null,
                "Cannot read properties of null (reading 'entry')",
            ),
            (
                object([]),
                "Cannot read properties of undefined (reading 'trim')",
            ),
            (
                object([("entry", JsValue::Null)]),
                "Cannot read properties of null (reading 'trim')",
            ),
            (
                object([("entry", JsValue::Number(3.0))]),
                "entry.trim is not a function",
            ),
            (
                object([("entry", JsValue::Bool(false))]),
                "entry.trim is not a function",
            ),
            (
                object([("entry", JsValue::Array(vec![]))]),
                "entry.trim is not a function",
            ),
            (
                object([("entry", object([]))]),
                "entry.trim is not a function",
            ),
        ] {
            let error = handle("grant_session_permission", &params, &interactions)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.code, "gateway_request_failed");
            assert_eq!(error.message, JsString::from(message));
        }
    }

    #[test]
    fn grant_session_permission_normalizes_deduplicates_and_echoes_original_entry() {
        use crate::config::permission::GatewaySessionPermissionRuleSetRegistry;
        use crate::runtime::interaction::InteractionOptions;
        let registry = Arc::new(GatewaySessionPermissionRuleSetRegistry::new());
        let interactions = GatewayInteractionCoordinator::new(InteractionOptions {
            permission_grants: Some(registry.clone()),
            ..Default::default()
        });
        let owner = JsString::from_utf16(vec![32, 0xd800, 32]);
        for entry in ["", "\u{feff} \u{2029}"] {
            let result = handle(
                "grant_session_permission",
                &object([("entry", entry.into())]),
                &interactions,
            )
            .unwrap()
            .unwrap();
            assert_eq!(result, object([("granted", JsValue::Bool(false))]));
            assert_eq!(registry.size(), 0);
        }
        for entry in [
            " Write ",
            "write_file",
            "Bash( echo hi:there )",
            ":pattern",
            "\u{feff}Read\u{feff}",
        ] {
            let params = object([
                ("sessionKey", JsValue::String(owner.clone())),
                ("entry", entry.into()),
            ]);
            let result = handle("grant_session_permission", &params, &interactions)
                .unwrap()
                .unwrap();
            assert_eq!(
                result,
                object([("granted", JsValue::Bool(true)), ("entry", entry.into())])
            );
        }
        let rules = interactions.session_allow_rules(&owner).snapshot();
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].tool_name, JsString::from("write_file"));
        assert_eq!(rules[1].tool_name, JsString::from("bash"));
        assert_eq!(rules[1].pattern, Some("echo hi:there".into()));
        assert_eq!(rules[2].tool_name, JsString::from(":pattern"));
        assert_eq!(rules[2].pattern, Some("pattern".into()));
        assert_eq!(rules[3].tool_name, JsString::from("read_file"));
        assert!(
            rules
                .iter()
                .all(|r| r.source.equals_str("session") && r.behavior.equals_str("allow"))
        );
        assert!(
            interactions
                .session_allow_rules(&JsString::from_utf16(vec![0xd800]))
                .is_empty()
        );
        let lease = registry.acquire(&owner, None);
        assert_eq!(lease.rules().snapshot().allow, rules);
        lease.release();
        assert_eq!(registry.size(), 1, "pre-session grants stay pinned");
        interactions.close_session(&owner, &"closed".into());
        assert_eq!(registry.size(), 0);
    }

    #[test]
    fn grant_session_permission_preserves_wire_map_keys_without_string_coercion() {
        use crate::config::permission::GatewaySessionPermissionRuleSetRegistry;
        use crate::runtime::interaction::InteractionOptions;
        let registry = Arc::new(GatewaySessionPermissionRuleSetRegistry::new());
        let interactions = GatewayInteractionCoordinator::new(InteractionOptions {
            permission_grants: Some(registry.clone()),
            ..Default::default()
        });
        for (params, size) in [
            (object([("entry", "Read".into())]), 1),
            (
                object([("sessionKey", JsValue::Null), ("entry", "Read".into())]),
                2,
            ),
            (
                object([
                    ("sessionKey", JsValue::Number(7.0)),
                    ("entry", "Read".into()),
                ]),
                3,
            ),
            (
                object([("sessionKey", "7".into()), ("entry", "Write".into())]),
                4,
            ),
            (
                object([
                    ("sessionKey", JsValue::Number(7.0)),
                    ("entry", "Read".into()),
                ]),
                4,
            ),
            (
                object([("sessionKey", object([])), ("entry", "Read".into())]),
                5,
            ),
            (
                object([("sessionKey", object([])), ("entry", "Read".into())]),
                6,
            ),
            (
                object([
                    ("sessionKey", JsValue::Array(vec![])),
                    ("entry", "Read".into()),
                ]),
                7,
            ),
            (
                object([("sessionKey", "".into()), ("entry", "Read".into())]),
                8,
            ),
        ] {
            let result = handle("grant_session_permission", &params, &interactions)
                .unwrap()
                .unwrap();
            assert_eq!(result.get("granted"), Some(&JsValue::Bool(true)));
            assert_eq!(registry.size(), size);
        }
        assert_eq!(
            registry.allow_rules(&"7".into()).snapshot()[0].tool_name,
            JsString::from("write_file")
        );
        assert!(registry.allow_rules(&"null".into()).is_empty());
        assert!(registry.allow_rules(&"undefined".into()).is_empty());
        assert!(registry.allow_rules(&"[object Object]".into()).is_empty());
        interactions.dispose(&"disposed".into());
        assert_eq!(registry.size(), 0);
    }

    #[test]
    fn permission_list_matches_ts_validation_including_null_and_non_string() {
        let interactions = GatewayInteractionCoordinator::default();
        for (params, code, message) in [
            (
                JsValue::Undefined,
                "gateway_request_failed",
                "Cannot read properties of undefined (reading 'sessionKey')",
            ),
            (
                JsValue::Null,
                "gateway_request_failed",
                "Cannot read properties of null (reading 'sessionKey')",
            ),
            (
                object([]),
                "INVALID_PERMISSION_LIST",
                "sessionKey is required.",
            ),
            (
                object([("sessionKey", JsValue::Null)]),
                "INVALID_PERMISSION_LIST",
                "sessionKey is required.",
            ),
            (
                object([("sessionKey", "\u{feff}\u{2029} ".into())]),
                "INVALID_PERMISSION_LIST",
                "sessionKey is required.",
            ),
        ] {
            let error = handle("permission_list", &params, &interactions)
                .unwrap()
                .unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(error.message, JsString::from(message));
        }
        for value in [
            JsValue::Bool(false),
            JsValue::Number(3.0),
            object([]),
            JsValue::Array(vec![]),
        ] {
            let error = handle(
                "permission_list",
                &object([("sessionKey", value)]),
                &interactions,
            )
            .unwrap()
            .unwrap_err();
            assert_eq!(error.code, "gateway_request_failed");
            assert_eq!(
                error.message,
                JsString::from("input.sessionKey?.trim is not a function")
            );
        }
        assert!(handle("not_permission", &JsValue::Null, &interactions).is_none());
    }

    #[test]
    fn permission_list_projects_live_pending_without_leaking_binding_or_consuming() {
        let interactions = GatewayInteractionCoordinator::default();
        let owner = JsString::from_utf16(vec![32, 0xd800, 32]);
        let answer = Arc::new(Mutex::new(None));
        let resolve = answer.clone();
        let registration = interactions
            .permission_bus()
            .register(
                &owner,
                PendingEntry {
                    request_id: "p".into(),
                    tool_call_id: "t".into(),
                    tool_name: "write_file".into(),
                    resolve: Arc::new(move |value| *resolve.lock().unwrap() = Some(value)),
                    reject: Arc::new(|_| panic!("list must not reject")),
                },
                Some(parse_json(r#"{"nested":{"path":"report.md"}}"#).unwrap()),
            )
            .unwrap();
        let params = object([("sessionKey", JsValue::String(owner.clone()))]);
        let list = || {
            handle("permission_list", &params, &interactions)
                .unwrap()
                .unwrap()
        };
        let expected = parse_json(r#"{"requests":[{"requestId":"p","toolCallId":"t","toolName":"write_file","payload":{"nested":{"path":"report.md"}}}]}"#).unwrap();
        assert_eq!(list(), expected);
        let JsValue::Object(ref mut changed) = list() else {
            unreachable!()
        };
        changed.insert("requests".into(), JsValue::Array(vec![]));
        assert_eq!(list(), expected);
        assert!(registration.active());
        assert!(answer.lock().unwrap().is_none());
        for session in [JsString::from("other"), JsString::from_utf16(vec![0xd800])] {
            assert_eq!(
                handle(
                    "permission_list",
                    &object([("sessionKey", JsValue::String(session))]),
                    &interactions
                )
                .unwrap()
                .unwrap(),
                object([("requests", JsValue::Array(vec![]))])
            );
        }
        assert!(interactions.decide_permission(
            &owner,
            PermissionDecision {
                request_id: "p".into(),
                decision: "allow".into(),
                remember: None,
                reason: None
            },
            None
        ));
        assert!(!registration.active());
        assert!(answer.lock().unwrap().is_some());
        assert_eq!(list(), object([("requests", JsValue::Array(vec![]))]));
    }

    #[test]
    fn permission_list_omits_absent_payload_and_observes_timeout_disposal() {
        let interactions = GatewayInteractionCoordinator::default();
        for (payload, expected_payload) in [
            (None, None),
            (Some(JsValue::Undefined), None),
            (Some(JsValue::Null), Some(JsValue::Null)),
        ] {
            let registration = interactions
                .permission_bus()
                .register(
                    &"s".into(),
                    PendingEntry {
                        request_id: "p".into(),
                        tool_call_id: "".into(),
                        tool_name: "".into(),
                        resolve: Arc::new(|_| panic!("query must not settle")),
                        reject: Arc::new(|_| {}),
                    },
                    payload,
                )
                .unwrap();
            let result = handle(
                "permission_list",
                &object([("sessionKey", "s".into())]),
                &interactions,
            )
            .unwrap()
            .unwrap();
            let JsValue::Array(requests) = result.get("requests").unwrap() else {
                unreachable!()
            };
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].get("payload").cloned(), expected_payload);
            registration.dispose(); // Same exact-registration cleanup used by timeout/abort owners.
            assert_eq!(
                handle(
                    "permission_list",
                    &object([("sessionKey", "s".into())]),
                    &interactions
                )
                .unwrap()
                .unwrap(),
                object([("requests", JsValue::Array(vec![]))])
            );
        }
    }
}
