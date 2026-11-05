use super::super::requests::Requests;
use crate::runtime::{
    connection::{Call, RequestError},
    interaction::{InteractionBinding, PermissionDecision, reconnect_result},
};
use pilotdeck_protocol::{JsString, JsValue};

// Wire names are the WebSocket frame method names from the TS frames registry.
// The connection layer already injects interactionBinding (and applies the
// sdkControlConnection rule for permission_decide) before dispatch
// (runtime/connection/context.rs); this family routes the three interaction
// round-trips to the injected GatewayInteractionCoordinator instead of the
// generic metadata fallback.
pub(super) const METHODS: &[&str] = &[
    "reconnect_interaction",
    "elicitation_respond",
    "permission_decide",
];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::interaction_request);

// JS truthiness, matching the TS guards: !input.nextBinding
// (GatewayInteractionCoordinator.ts:74) and if (binding && ...)
// (GatewayElicitationBus.ts:104) treat 0/"", null, undefined and NaN as absent.
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

// TS never validates a previous binding (normalizeBinding runs only on the
// next binding, InteractionReconnect.ts:122-125); a truthy-but-malformed value
// simply fails every sameBinding comparison because `.connectionId` reads as
// undefined. A validated stored binding always has a non-empty connectionId,
// so the empty-id binding below can never equal one.
fn never_matching() -> InteractionBinding {
    InteractionBinding {
        connection_id: JsString::from(""),
        generation: 0,
    }
}

// A truthy-but-malformed interactionBinding is a non-current binding in TS
// (isCurrent → sameBinding reads `.connectionId` → undefined), so it fails
// the consume gate with delivered:false instead of erroring.
fn request_binding(params: &JsValue) -> Option<InteractionBinding> {
    let value = params
        .get("interactionBinding")
        .filter(|value| truthy(value))?;
    Some(InteractionBinding::from_value(value).unwrap_or_else(|_| never_matching()))
}

impl Requests {
    pub(in crate::native_process) fn interaction_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "elicitation_respond" => Ok(self.elicitation_respond(&params)),
                "permission_decide" => Ok(self.permission_decide(&params)),
                _ => self.reconnect_interaction(&params),
            }
        })
    }
    // TS reconnectInteraction → coordinator.reconnect
    // (GatewayInteractionCoordinator.ts:72-76): a falsy nextBinding
    // short-circuits to the stale_binding result; a truthy-but-malformed one
    // throws from normalizeBinding (InteractionReconnect.ts:75-84) and the
    // WS layer maps that plain Error to gateway_request_failed.
    fn reconnect_interaction(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let Some(next_value) = params.get("nextBinding").filter(|value| truthy(value)) else {
            return Ok(reconnect_result("stale_binding", None, vec![]));
        };
        let next = InteractionBinding::from_value(next_value).map_err(RequestError::new)?;
        let previous = params
            .get("previousBinding")
            .filter(|value| truthy(value))
            .map(|value| {
                InteractionBinding::from_value(value).unwrap_or_else(|_| never_matching())
            });
        let Some(owner) = params.get("sessionKey").and_then(JsValue::as_string) else {
            // A non-string sessionKey is outside the port's string owner
            // domain; the runtime port owns that outcome rule (#1632 item 8,
            // A7.8-INTERACTION-NONSTRING-OWNER-001). The adapter only maps the
            // owner, it does not decide what the miss looks like.
            return self
                .interactions
                .reconnect_unkeyed(next, previous.as_ref())
                .map_err(RequestError::new);
        };
        self.interactions
            .reconnect(owner, Some(next), previous.as_ref())
            .map_err(RequestError::new)
    }
    // TS respondElicitation performs no input validation
    // (InProcessGateway.ts:2626-2636): consume looks the pending entry up with
    // Map.get (SameValueZero, no string coercion), so a non-string
    // sessionKey/requestId is a miss and the answer is resolved verbatim.
    fn elicitation_respond(&self, params: &JsValue) -> JsValue {
        let delivered = match (
            params.get("sessionKey").and_then(JsValue::as_string),
            params.get("requestId").and_then(JsValue::as_string),
        ) {
            (Some(owner), Some(request)) => {
                let binding = request_binding(params);
                let answer = params.get("answer").cloned().unwrap_or(JsValue::Undefined);
                self.interactions
                    .respond_elicitation(owner, request, answer, binding.as_ref())
            }
            _ => false,
        };
        crate::metadata::object([("delivered", JsValue::Bool(delivered))])
    }
    // TS permissionDecide → decidePermission (InProcessGateway.ts:2776-2778):
    // no input validation, same Map.get miss semantics as elicitation, and
    // decision/remember/reason are resolved to the pending entry verbatim.
    fn permission_decide(&self, params: &JsValue) -> JsValue {
        let delivered = match (
            params.get("sessionKey").and_then(JsValue::as_string),
            params.get("requestId").and_then(JsValue::as_string),
        ) {
            (Some(owner), Some(request)) => {
                let binding = request_binding(params);
                let decision = PermissionDecision {
                    request_id: request.clone(),
                    decision: params
                        .get("decision")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                    remember: params.get("remember").cloned(),
                    reason: params.get("reason").cloned(),
                };
                self.interactions
                    .decide_permission(owner, decision, binding.as_ref())
            }
            _ => false,
        };
        crate::metadata::object([("delivered", JsValue::Bool(delivered))])
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        metadata::object, native_process::management_tests::setup, negative_assert_eq,
        runtime::interaction::PendingEntry, runtime::turn::gateway::GatewayRequests,
    };
    use pilotdeck_protocol::{JsString, JsValue};
    use std::sync::{Arc, Mutex};

    type Sink = Arc<Mutex<Vec<JsValue>>>;

    fn pending(request_id: &str, sink: &Sink) -> PendingEntry {
        let resolved = sink.clone();
        let rejected = sink.clone();
        PendingEntry {
            request_id: request_id.into(),
            tool_call_id: "tc".into(),
            tool_name: "fixture".into(),
            resolve: Arc::new(move |value| resolved.lock().unwrap().push(value)),
            reject: Arc::new(move |error| {
                rejected
                    .lock()
                    .unwrap()
                    .push(JsValue::String(JsString::from_utf16(
                        "reject:"
                            .encode_utf16()
                            .chain(error.units().iter().copied())
                            .collect(),
                    )))
            }),
        }
    }

    async fn full_path(
        requests: &crate::native_process::requests::Requests,
        method: &str,
        params: JsValue,
    ) -> Result<JsValue, crate::runtime::connection::RequestError> {
        GatewayRequests::request(requests, JsString::from(method), params).await
    }

    fn outcome_is(result: &JsValue, expected: &str) -> bool {
        result
            .get("outcome")
            .and_then(JsValue::as_string)
            .is_some_and(|outcome| outcome.equals_str(expected))
    }

    fn binding_connection_is(result: &JsValue, expected: &str) -> bool {
        result
            .get("binding")
            .and_then(|binding| binding.get("connectionId"))
            .and_then(JsValue::as_string)
            .is_some_and(|id| id.equals_str(expected))
    }

    async fn teardown(requests: &crate::native_process::requests::Requests) {
        requests.owner.shutdown().await.expect("#1316 shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1316 project dispose");
    }

    // The three wire names reach the interaction family through the full
    // request path, not the generic metadata fallback whose
    // CAPABILITY_UNAVAILABLE message is
    // "Native gateway method X is not configured.".
    #[tokio::test]
    async fn elicitation_respond_delivers_registered_request() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let sink: Sink = Arc::default();
        requests
            .interactions
            .elicitation_bus()
            .register(
                &"s1".into(),
                pending("r1", &sink),
                Some(object([("k", "v".into())])),
            )
            .expect("#1316 register elicitation");
        let answer = object([("behavior", "answered".into())]);
        let params = object([
            ("sessionKey", "s1".into()),
            ("requestId", "r1".into()),
            ("answer", answer.clone()),
        ]);
        let result = full_path(&requests, "elicitation_respond", params)
            .await
            .expect("#1316 elicitation delivered");
        negative_assert_eq!(
            result.get("delivered").and_then(JsValue::as_bool),
            Some(true),
            "#1316 elicitation_respond delivered"
        );
        negative_assert_eq!(
            sink.lock().unwrap().len(),
            1,
            "#1316 elicitation resolved exactly once"
        );
        let answered: JsString = "answered".into();
        negative_assert_eq!(
            sink.lock().unwrap()[0]
                .get("behavior")
                .and_then(JsValue::as_string),
            Some(&answered),
            "#1316 elicitation answer resolved verbatim"
        );
        teardown(&requests).await;
    }

    #[tokio::test]
    async fn elicitation_respond_unknown_request_reports_undelivered() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let sink: Sink = Arc::default();
        let params = object([
            ("sessionKey", "s1".into()),
            ("requestId", "missing".into()),
            ("answer", "no".into()),
        ]);
        let result = full_path(&requests, "elicitation_respond", params)
            .await
            .expect("#1316 elicitation undelivered still resolves");
        negative_assert_eq!(
            result.get("delivered").and_then(JsValue::as_bool),
            Some(false),
            "#1316 elicitation_respond unknown request delivered:false"
        );
        negative_assert_eq!(
            sink.lock().unwrap().len(),
            0,
            "#1316 unknown elicitation resolves nothing"
        );
        teardown(&requests).await;
    }

    // TS consume looks entries up with Map.get (SameValueZero, no string
    // coercion): a non-string sessionKey can never equal the registered
    // string owner, so the response is delivered:false, not an error.
    #[tokio::test]
    async fn permission_decide_non_string_session_key_is_a_miss() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let sink: Sink = Arc::default();
        requests
            .interactions
            .permission_bus()
            .register(&"s1".into(), pending("r1", &sink), None)
            .expect("#1316 register permission");
        let params = object([
            ("sessionKey", JsValue::Number(5.0)),
            ("requestId", "r1".into()),
            ("decision", "allow".into()),
        ]);
        let result = full_path(&requests, "permission_decide", params)
            .await
            .expect("#1316 non-string sessionKey still resolves");
        negative_assert_eq!(
            result.get("delivered").and_then(JsValue::as_bool),
            Some(false),
            "#1316 permission_decide non-string sessionKey delivered:false"
        );
        negative_assert_eq!(
            sink.lock().unwrap().len(),
            0,
            "#1316 non-string sessionKey resolves nothing"
        );
        teardown(&requests).await;
    }

    // TS decidePermission resolves {requestId, decision, remember, reason}
    // verbatim (GatewayInteractionCoordinator.ts:118-123); a non-string
    // decision is delivered as-is.
    #[tokio::test]
    async fn permission_decide_passes_decision_through_verbatim() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let sink: Sink = Arc::default();
        requests
            .interactions
            .permission_bus()
            .register(&"s1".into(), pending("r1", &sink), None)
            .expect("#1316 register permission");
        let params = object([
            ("sessionKey", "s1".into()),
            ("requestId", "r1".into()),
            ("decision", JsValue::Number(5.0)),
            ("remember", JsValue::Bool(true)),
        ]);
        let result = full_path(&requests, "permission_decide", params)
            .await
            .expect("#1316 permission delivered");
        negative_assert_eq!(
            result.get("delivered").and_then(JsValue::as_bool),
            Some(true),
            "#1316 permission_decide delivered"
        );
        let resolved = sink.lock().expect("#1316 permission sink")[0].clone();
        negative_assert_eq!(
            resolved
                .get("requestId")
                .and_then(JsValue::as_string)
                .is_some_and(|id| id.equals_str("r1")),
            true,
            "#1316 resolved requestId"
        );
        negative_assert_eq!(
            resolved.get("decision"),
            Some(&JsValue::Number(5.0)),
            "#1316 non-string decision resolved verbatim"
        );
        negative_assert_eq!(
            resolved.get("remember"),
            Some(&JsValue::Bool(true)),
            "#1316 remember resolved verbatim"
        );
        negative_assert_eq!(
            resolved.get("reason"),
            Some(&JsValue::Undefined),
            "#1316 absent reason stays undefined"
        );
        teardown(&requests).await;
    }

    // A truthy-but-malformed interactionBinding fails the isCurrent gate in
    // TS (sameBinding reads .connectionId → undefined), so consume misses
    // with delivered:false rather than erroring.
    #[tokio::test]
    async fn permission_decide_malformed_binding_is_a_miss() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let sink: Sink = Arc::default();
        requests
            .interactions
            .permission_bus()
            .register(&"s1".into(), pending("r1", &sink), None)
            .expect("#1316 register permission");
        let params = object([
            ("sessionKey", "s1".into()),
            ("requestId", "r1".into()),
            ("decision", "allow".into()),
            ("interactionBinding", object([("connectionId", "c".into())])),
        ]);
        let result = full_path(&requests, "permission_decide", params)
            .await
            .expect("#1316 malformed binding still resolves");
        negative_assert_eq!(
            result.get("delivered").and_then(JsValue::as_bool),
            Some(false),
            "#1316 malformed interactionBinding delivered:false"
        );
        negative_assert_eq!(
            sink.lock().unwrap().len(),
            0,
            "#1316 malformed binding resolves nothing"
        );
        teardown(&requests).await;
    }

    #[tokio::test]
    async fn reconnect_interaction_replays_pending_requests() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let sink: Sink = Arc::default();
        requests
            .interactions
            .permission_bus()
            .register(
                &"s1".into(),
                pending("r1", &sink),
                Some(object([("k", "v".into())])),
            )
            .expect("#1316 register permission");
        let next = object([
            ("connectionId", "c1".into()),
            ("generation", JsValue::Number(1.0)),
        ]);
        let params = object([("sessionKey", "s1".into()), ("nextBinding", next.clone())]);
        let result = full_path(&requests, "reconnect_interaction", params)
            .await
            .expect("#1316 first reconnect");
        negative_assert_eq!(
            outcome_is(&result, "initial"),
            true,
            "#1316 first reconnect outcome initial"
        );
        negative_assert_eq!(
            binding_connection_is(&result, "c1"),
            true,
            "#1316 reconnect returns the next binding"
        );
        let replayed = match result.get("requests") {
            Some(JsValue::Array(items)) => Some(items.len()),
            _ => None,
        };
        negative_assert_eq!(replayed, Some(1), "#1316 pending request replayed");
        // Same binding again without a previous binding: reconnected.
        let params = object([("sessionKey", "s1".into()), ("nextBinding", next.clone())]);
        let result = full_path(&requests, "reconnect_interaction", params)
            .await
            .expect("#1316 repeat reconnect");
        negative_assert_eq!(
            outcome_is(&result, "reconnected"),
            true,
            "#1316 same binding without previous is reconnected"
        );
        // Wrong previous binding: stale.
        let params = object([
            ("sessionKey", "s1".into()),
            (
                "nextBinding",
                object([
                    ("connectionId", "c2".into()),
                    ("generation", JsValue::Number(2.0)),
                ]),
            ),
            (
                "previousBinding",
                object([
                    ("connectionId", "other".into()),
                    ("generation", JsValue::Number(9.0)),
                ]),
            ),
        ]);
        let result = full_path(&requests, "reconnect_interaction", params)
            .await
            .expect("#1316 stale reconnect");
        negative_assert_eq!(
            outcome_is(&result, "stale_binding"),
            true,
            "#1316 wrong previous binding is stale"
        );
        teardown(&requests).await;
    }

    // TS coordinator short-circuit: !input.nextBinding → stale_binding.
    #[tokio::test]
    async fn reconnect_interaction_falsy_next_binding_is_stale() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let params = object([("sessionKey", "s1".into())]);
        let result = full_path(&requests, "reconnect_interaction", params)
            .await
            .expect("#1316 falsy nextBinding still resolves");
        negative_assert_eq!(
            outcome_is(&result, "stale_binding"),
            true,
            "#1316 missing nextBinding is stale_binding"
        );
        teardown(&requests).await;
    }

    // TS normalizeBinding throws for a truthy-but-malformed next binding and
    // the WS layer maps that plain Error to gateway_request_failed.
    #[tokio::test]
    async fn reconnect_interaction_malformed_next_binding_errors() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let params = object([
            ("sessionKey", "s1".into()),
            (
                "nextBinding",
                object([("generation", JsValue::Number(1.0))]),
            ),
        ]);
        let error = full_path(&requests, "reconnect_interaction", params)
            .await
            .expect_err("#1316 malformed nextBinding errors");
        negative_assert_eq!(
            error.code.as_str(),
            "gateway_request_failed",
            "#1316 malformed nextBinding error code"
        );
        teardown(&requests).await;
    }

    // A non-string sessionKey has no representation in the port's string key
    // domain, so the port walks its fresh-owner path (no stored binding, no
    // pending requests) and never persists state under such an owner.
    //
    // This is the *Rust* side of an unapproved divergence. The recorded TS
    // oracle for the same input (`tests/fixtures/connection-context/
    // oracle.json`, scenario `session-key-5`) answers `reconnected`: TS keys
    // the binding by the raw value, so a `submit_turn` under `sessionKey: 5`
    // makes even the first `reconnect_interaction(5)` reconnect. Registered as
    // A7.8-INTERACTION-NONSTRING-OWNER-001 (correction in #1703), tracked in
    // #1465. Closing the divergence means widening the owner key domain, which
    // is that ledger round's decision, not this port extraction's.
    #[tokio::test]
    async fn reconnect_interaction_non_string_session_key_stays_fresh() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let params = object([
            ("sessionKey", JsValue::Number(5.0)),
            (
                "nextBinding",
                object([
                    ("connectionId", "c1".into()),
                    ("generation", JsValue::Number(1.0)),
                ]),
            ),
        ]);
        let result = full_path(&requests, "reconnect_interaction", params)
            .await
            .expect("#1316 non-string sessionKey still resolves");
        negative_assert_eq!(
            outcome_is(&result, "no_pending"),
            true,
            "#1316 non-string sessionKey no_pending"
        );
        let params = object([
            ("sessionKey", JsValue::Number(5.0)),
            (
                "nextBinding",
                object([
                    ("connectionId", "c1".into()),
                    ("generation", JsValue::Number(1.0)),
                ]),
            ),
            (
                "previousBinding",
                object([
                    ("connectionId", "old".into()),
                    ("generation", JsValue::Number(0.0)),
                ]),
            ),
        ]);
        let result = full_path(&requests, "reconnect_interaction", params)
            .await
            .expect("#1316 non-string sessionKey with previous still resolves");
        negative_assert_eq!(
            outcome_is(&result, "stale_binding"),
            true,
            "#1316 non-string sessionKey with previous is stale"
        );
        teardown(&requests).await;
    }

    /// Pins the *evidence* of the divergence above, not the Rust behaviour:
    /// the recorded TS frames for scenario `session-key-5` answer the first
    /// `reconnect_interaction(5)` after a `submit_turn(sessionKey: 5)` with
    /// `reconnected`.
    ///
    /// If the fixture is ever re-recorded and TS stops storing a binding for a
    /// non-string key, the divergence is gone — and this test says so instead
    /// of leaving the claim to prose (#1465 / #1703).
    ///
    /// The fixture is scanned as text on purpose: it records lone-surrogate
    /// escapes verbatim, which strict JSON parsers reject. Scenario blocks are
    /// delimited by the pretty-printer's six-space `"name"` key.
    #[test]
    fn recorded_ts_oracle_reconnects_a_non_string_session_key() {
        const ORACLE: &str = include_str!("../../../tests/fixtures/connection-context/oracle.json");
        let scenario = ORACLE
            .split("\n      \"name\": \"")
            .find(|block| block.starts_with("session-key-5\""))
            .expect("#1703 the session-key-5 scenario");
        negative_assert_eq!(
            scenario.contains("\\\"id\\\":\\\"reconnect_interaction\\\""),
            true,
            "#1703 the recorded reconnect response"
        );
        negative_assert_eq!(
            scenario.contains("\\\"outcome\\\":\\\"reconnected\\\""),
            true,
            "#1703 TS reconnects a non-string sessionKey by raw value"
        );
    }
}
