use super::super::requests::Requests;
use crate::{
    metadata::object,
    runtime::connection::{Call, RequestError},
};
use pilotdeck_protocol::JsValue;

// Wire names are the WebSocket frame method names from the TS frames registry.
// The 2026-11-05 #1475 deferral ("answer CAPABILITY_UNAVAILABLE honestly") was
// lifted on 2026-11-08, so the three methods now carry the TS "local gateway
// without an injected always-on control" behavior: a resolved result envelope
// whose `error` field carries `code: "not_configured"`
// (InProcessGateway.ts:2992-3016), which the WS boundary sends as `ok: true`
// (GatewayWsConnection.ts:186-187) — not a request-level error. The control
// port itself — the fallback apply handler and the always-on manager — lands
// with the background runtime in #2294, so this process has no control
// concept and every call takes the unconfigured branch.
pub(super) const METHODS: &[&str] = &["always_on_apply", "always_on_abort", "always_on_rerun_plan"];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::always_on_request);

// The `error` field of a not-configured envelope, in the TS object literal's
// key order: code, then message.
fn not_configured_error(message: &str) -> JsValue {
    object([
        ("code", "not_configured".into()),
        ("message", message.into()),
    ])
}

impl Requests {
    pub(in crate::native_process) fn always_on_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                // The only arm that reads `input`: the unconfigured abort
                // echoes `input.sessionKey` in its return object
                // (InProcessGateway.ts:3001), so a nullish `params` throws the
                // boundary TypeError — `input_property` reproduces the message
                // and `RequestError::new` the `gateway_request_failed` wire
                // code the TS catch-all produces (GatewayWsConnection.ts:
                // 246-254). A present non-undefined value is echoed verbatim
                // (string, number and null alike), and a missing key is
                // dropped, matching JSON.stringify over `sessionKey:
                // undefined`.
                "always_on_abort" => {
                    let session_key = super::input_property(&params, "sessionKey")?;
                    let mut envelope = vec![("aborted", JsValue::Bool(false))];
                    if let Some(value) = session_key.filter(|value| !value.is_undefined()) {
                        envelope.push(("sessionKey", value));
                    }
                    envelope.push((
                        "error",
                        not_configured_error("Always-On abort is not configured on this gateway."),
                    ));
                    Ok(object(envelope))
                }
                // Neither remaining arm reads `input` in the unconfigured
                // branch (InProcessGateway.ts:2992-2997, :3010-3013), so any
                // params shape — including null and undefined — still answers
                // the envelope.
                "always_on_rerun_plan" => Ok(object([
                    ("runId", "".into()),
                    (
                        "error",
                        not_configured_error("Always-On rerun is not configured on this gateway."),
                    ),
                ])),
                _ => Ok(object([
                    ("sessionKey", "".into()),
                    (
                        "error",
                        not_configured_error("Always-On apply is not configured on this gateway."),
                    ),
                ])),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::turn::gateway::GatewayRequests;
    use crate::{native_process::management_tests::setup, negative_assert_eq};
    use pilotdeck_protocol::{JsString, JsValue, stringify_json};

    async fn teardown(requests: &crate::native_process::requests::Requests) {
        requests.owner.shutdown().await.expect("#1313 shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1313 project dispose");
    }

    // The unconfigured state answers the TS result envelopes byte for byte:
    // JSON.stringify of the InProcessGateway returns, in the same key order
    // (ts-probe output.txt, Node v22.22.0 baseline).
    #[tokio::test]
    async fn always_on_methods_answer_not_configured_envelopes() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1313 fixture setup");
        // No `projectKey` here: an unserved key is refused by the shared
        // admission gate before the family runs (covered by its own test
        // below); the TS probe carried one only because the local gateway has
        // no such gate.
        let full =
            |fields: &[(&'static str, JsValue)]| crate::metadata::object(fields.iter().cloned());
        for (method, params, envelope) in [
            (
                "always_on_apply",
                full(&[("workCycleId", "wc1".into()), ("projectName", "n".into())]),
                "{\"sessionKey\":\"\",\"error\":{\"code\":\"not_configured\",\"message\":\
                 \"Always-On apply is not configured on this gateway.\"}}",
            ),
            (
                "always_on_apply",
                JsValue::Object(Default::default()),
                "{\"sessionKey\":\"\",\"error\":{\"code\":\"not_configured\",\"message\":\
                 \"Always-On apply is not configured on this gateway.\"}}",
            ),
            (
                "always_on_abort",
                full(&[("sessionKey", "s1".into())]),
                "{\"aborted\":false,\"sessionKey\":\"s1\",\"error\":{\"code\":\"not_configured\",\
                 \"message\":\"Always-On abort is not configured on this gateway.\"}}",
            ),
            (
                "always_on_abort",
                full(&[("reason", "stop".into())]),
                "{\"aborted\":false,\"error\":{\"code\":\"not_configured\",\"message\":\
                 \"Always-On abort is not configured on this gateway.\"}}",
            ),
            (
                "always_on_abort",
                full(&[("sessionKey", JsValue::Number(42.0))]),
                "{\"aborted\":false,\"sessionKey\":42,\"error\":{\"code\":\"not_configured\",\
                 \"message\":\"Always-On abort is not configured on this gateway.\"}}",
            ),
            (
                "always_on_rerun_plan",
                full(&[("planId", "pl1".into())]),
                "{\"runId\":\"\",\"error\":{\"code\":\"not_configured\",\"message\":\
                 \"Always-On rerun is not configured on this gateway.\"}}",
            ),
            (
                "always_on_rerun_plan",
                JsValue::Object(Default::default()),
                "{\"runId\":\"\",\"error\":{\"code\":\"not_configured\",\"message\":\
                 \"Always-On rerun is not configured on this gateway.\"}}",
            ),
        ] {
            let result = GatewayRequests::request(&requests, JsString::from(method), params).await;
            let value = match result {
                Ok(value) => value,
                Err(error) => panic!(
                    "#1313 {method} message: unconfigured answers an envelope, got {error:?}"
                ),
            };
            let text = stringify_json(&value)
                .expect("#1313 envelope serializes")
                .expect("#1313 envelope is defined");
            negative_assert_eq!(text, envelope, "#1313 {method} message");
        }
        teardown(&requests).await;
    }

    // The abort arm reads `input.sessionKey`; apply and rerun do not. A nullish
    // `params` therefore only fails abort, with the boundary TypeError message
    // and `gateway_request_failed` code (ts-probe output.txt L9-10;
    // GatewayWsConnection.ts:246-254), while apply and rerun still answer.
    #[tokio::test]
    async fn always_on_abort_nullish_params_hit_the_session_key_read() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1313 fixture setup");
        for (params, origin) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
            let result =
                GatewayRequests::request(&requests, JsString::from("always_on_abort"), params)
                    .await;
            let error = result.expect_err("#1313 abort nullish answers the TypeError");
            negative_assert_eq!(
                error.code.as_str(),
                "gateway_request_failed",
                "#1313 abort nullish {origin} code"
            );
            negative_assert_eq!(
                error.message.to_utf8().expect("#1313 message UTF-8"),
                format!("Cannot read properties of {origin} (reading 'sessionKey')"),
                "#1313 abort nullish {origin} message"
            );
        }
        for method in ["always_on_apply", "always_on_rerun_plan"] {
            for params in [JsValue::Null, JsValue::Undefined] {
                let result =
                    GatewayRequests::request(&requests, JsString::from(method), params).await;
                let value = result
                    .unwrap_or_else(|error| panic!("#1313 {method} never reads input: {error:?}"));
                let text = stringify_json(&value)
                    .expect("#1313 envelope serializes")
                    .expect("#1313 envelope is defined");
                negative_assert_eq!(
                    text.contains("\"code\":\"not_configured\""),
                    true,
                    "#1313 {method} nullish still answers the not_configured envelope"
                );
            }
        }
        teardown(&requests).await;
    }

    // The always-on rounds walk the same shared admission gate as every other
    // session request: an unserved projectKey is refused by the entry gate
    // before the family runs, and the configured project is admitted through
    // to the envelope (issue acceptance: "always-on 轮次走同一接纳器").
    #[tokio::test]
    async fn always_on_rounds_share_the_admission_gate() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1313 fixture setup");
        let params = crate::metadata::object([("projectKey", "/wrong/project".into())]);
        let result =
            GatewayRequests::request(&requests, JsString::from("always_on_apply"), params).await;
        negative_assert_eq!(
            result
                .expect_err("#1313 wrong project is refused")
                .code
                .as_str(),
            "gateway_request_failed",
            "#1313 always_on_apply admission gate refuses an unserved project"
        );
        let project_key = requests.options.project.to_string_lossy().into_owned();
        let params = crate::metadata::object([
            ("projectKey", project_key.as_str().into()),
            ("workCycleId", "wc1".into()),
        ]);
        let result =
            GatewayRequests::request(&requests, JsString::from("always_on_apply"), params).await;
        let value =
            result.unwrap_or_else(|error| panic!("#1313 served project is admitted: {error:?}"));
        negative_assert_eq!(
            value
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(JsValue::as_string),
            Some(&JsString::from("not_configured")),
            "#1313 always_on_apply admission gate admits the configured project"
        );
        teardown(&requests).await;
    }

    // The three names must stay out of describe: TS lists them only when an
    // `alwaysOnControl` is injected (InProcessGateway.ts:2123-2126), and this
    // process binds none — the runtime lands in #2294.
    #[tokio::test]
    async fn always_on_methods_are_not_advertised() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1313 fixture setup");
        let described = GatewayRequests::describe_server(&requests)
            .await
            .expect("#1313 describe");
        let capabilities = match described.get("capabilities") {
            Some(pilotdeck_protocol::JsValue::Array(items)) => items,
            _ => panic!("#1313 capability array"),
        };
        for method in ["always_on_apply", "always_on_abort", "always_on_rerun_plan"] {
            let advertised = capabilities.iter().any(|capability| {
                capability
                    .as_string()
                    .is_some_and(|name| name.equals_str(method))
            });
            negative_assert_eq!(
                advertised,
                false,
                "#1313 {method} must not be advertised in describe"
            );
        }
        teardown(&requests).await;
    }
}
