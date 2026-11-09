use super::*;
use crate::runtime::{interaction::InteractionBinding, refresh::RefreshRouter};

pub(super) fn describe(owner: &SessionOwner, run_registry: bool) -> JsValue {
    let mut capabilities = Vec::new();
    if run_registry {
        capabilities.extend(["run_get", "run_events", "run_reattach"]);
    }
    capabilities.extend([
        "delete_session",
        "session_transcript_archive",
        "seed_read_state",
    ]);
    object([
        ("mode", "in_process".into()),
        (
            "sessionCount",
            JsValue::Number(owner.cached_session_count() as f64),
        ),
        (
            "capabilities",
            JsValue::Array(capabilities.into_iter().map(JsValue::from).collect()),
        ),
    ])
}

pub(super) fn reconnect(
    interactions: &GatewayInteractionCoordinator,
    params: &JsValue,
) -> Result<JsValue, RequestError> {
    let empty = JsString::default();
    let session = match params.get("sessionKey") {
        None | Some(JsValue::Null | JsValue::Undefined) => &empty,
        Some(JsValue::String(session)) => session,
        _ => return Err(RequestError::new("sessionKey must be a string.")),
    };
    let binding = |name| {
        params
            .get(name)
            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            .map(InteractionBinding::from_value)
            .transpose()
            .map_err(RequestError::new)
    };
    // The WS boundary supplies nextBinding; the wire caller cannot choose it.
    let next = binding("nextBinding")?;
    let previous = binding("previousBinding")?;
    interactions
        .reconnect(session, next, previous.as_ref())
        .map_err(RequestError::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::interaction::{InteractionOptions, PendingEntry};

    fn binding(id: &str, generation: u64) -> JsValue {
        object([
            ("connectionId", id.into()),
            ("generation", JsValue::Number(generation as f64)),
        ])
    }

    #[test]
    fn reconnect_uses_pending_bus_and_preserves_stale_ownership() {
        let interactions = GatewayInteractionCoordinator::new(InteractionOptions::default());
        let registration = interactions
            .elicitation_bus()
            .register(
                &"session".into(),
                PendingEntry {
                    request_id: "question".into(),
                    tool_call_id: "call".into(),
                    tool_name: "ask".into(),
                    resolve: Arc::new(|_| {}),
                    reject: Arc::new(|_| {}),
                },
                Some(object([("question", "pending".into())])),
            )
            .unwrap();
        let first = reconnect(
            &interactions,
            &object([
                ("sessionKey", "session".into()),
                ("nextBinding", binding("first", 1)),
            ]),
        )
        .unwrap();
        assert_eq!(first.get("outcome"), Some(&"initial".into()));
        assert!(
            matches!(first.get("requests"), Some(JsValue::Array(requests)) if requests.len() == 1)
        );
        let stale = reconnect(
            &interactions,
            &object([
                ("sessionKey", "session".into()),
                ("nextBinding", binding("next", 2)),
            ]),
        )
        .unwrap();
        assert_eq!(stale.get("outcome"), Some(&"stale_binding".into()));
        assert_eq!(
            interactions
                .get_binding(&"session".into())
                .unwrap()
                .connection_id,
            JsString::from("first")
        );
        let next = reconnect(
            &interactions,
            &object([
                ("sessionKey", "session".into()),
                ("nextBinding", binding("next", 2)),
                ("previousBinding", binding("first", 1)),
            ]),
        )
        .unwrap();
        assert_eq!(next.get("outcome"), Some(&"reconnected".into()));
        registration.dispose();
    }

    #[test]
    fn absent_next_is_stale_and_malformed_binding_is_an_error() {
        let interactions = GatewayInteractionCoordinator::default();
        assert_eq!(
            reconnect(&interactions, &JsValue::Null)
                .unwrap()
                .get("outcome"),
            Some(&"stale_binding".into())
        );
        let invalid = object([("nextBinding", binding("", 1))]);
        assert!(reconnect(&interactions, &invalid).is_err());
    }
}
