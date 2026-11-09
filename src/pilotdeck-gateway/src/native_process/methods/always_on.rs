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
// port is published by the automation generation and can provide either
// a live manager or standalone apply; without a published control the
// unconfigured branch below preserves the TS envelopes.
pub(super) const METHODS: &[&str] = crate::runtime::always_on::ALWAYS_ON_METHODS;
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
            if let Some(control) = self.automation.as_ref().and_then(|slot| slot.control()) {
                let project: pilotdeck_protocol::JsString =
                    self.options.project.to_string_lossy().as_ref().into();
                return control
                    .dispatch(&method, &project, params)
                    .await
                    .map_err(|error| RequestError {
                        code: error.code,
                        message: error.message.into(),
                        details: None,
                        validation: None,
                    });
            }
            match method.as_str() {
                // The only arm that reads `input`: the unconfigured abort
                // echoes `input.sessionKey` in its return object
                // (InProcessGateway.ts:3003), so a nullish `params` throws the
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
    use crate::{native_process::management_tests::setup, negative_assert, negative_assert_eq};
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
            // A null sessionKey is echoed verbatim, not folded into the
            // missing-key case (ts-probe output.txt L8): the filter drops only
            // `undefined`, so this row pins the nullish-predicate boundary.
            (
                "always_on_abort",
                full(&[("sessionKey", JsValue::Null)]),
                "{\"aborted\":false,\"sessionKey\":null,\"error\":{\"code\":\"not_configured\",\
                 \"message\":\"Always-On abort is not configured on this gateway.\"}}",
            ),
            // The empty string rides the same echo: it is a present value, not
            // a falsy one to drop (MM fix1 boundary P2a — the predicate is
            // `!== undefined`, never a truthiness check).
            (
                "always_on_abort",
                full(&[("sessionKey", "".into())]),
                "{\"aborted\":false,\"sessionKey\":\"\",\"error\":{\"code\":\"not_configured\",\
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
                // Byte for byte with the rows above: the whole envelope is
                // the contract, not just the error code half of it.
                let envelope = if method == "always_on_apply" {
                    "{\"sessionKey\":\"\",\"error\":{\"code\":\"not_configured\",\"message\":\
                     \"Always-On apply is not configured on this gateway.\"}}"
                } else {
                    "{\"runId\":\"\",\"error\":{\"code\":\"not_configured\",\"message\":\
                     \"Always-On rerun is not configured on this gateway.\"}}"
                };
                negative_assert_eq!(text, envelope, "#1313 {method} nullish message");
            }
        }
        teardown(&requests).await;
    }

    // Fixed TS InProcessGateway does not inspect projectKey in the
    // unconfigured apply branch. The common native view still accepts each
    // key shape before the identical envelope is returned.
    #[tokio::test]
    async fn always_on_rounds_share_the_admission_gate() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1313 fixture setup");
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/native-multiproject-gates/oracle.json"
        ))
        .unwrap();
        let cases = match oracle.get("cases").unwrap() {
            JsValue::Array(cases) => cases,
            _ => panic!("cases"),
        };
        for (name, key) in [
            ("null-always", JsValue::Null),
            ("number-always", JsValue::Number(5.0)),
            ("foreign-always", JsValue::from("/wrong/project")),
        ] {
            let expected = cases
                .iter()
                .find(|case| {
                    case.get("id")
                        .and_then(JsValue::as_string)
                        .is_some_and(|id| id.equals_str(name))
                })
                .unwrap();
            let result = GatewayRequests::request(
                &requests,
                JsString::from("always_on_apply"),
                crate::metadata::object([("projectKey", key)]),
            )
            .await
            .unwrap();
            negative_assert_eq!(
                Some(&result),
                expected.get("value"),
                "#1313 unconfigured envelope matches fixed TS"
            );
        }
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
    /// A `Requests` whose always-on manager is staged for the fixture's one
    /// project, mirroring what the assembly publishes from a
    /// `alwaysOn: {projects: {<project>: {enabled: true}}}` section — the
    /// runtime is bound to nothing, so every control call answers the
    /// `not_ready` envelope the r1 golden pins.
    async fn configured_requests() -> crate::native_process::requests::Requests {
        let (_scratch, mut requests) =
            std::panic::catch_unwind(setup).expect("#2294 fixture setup");
        let project_key = requests.options.project.to_string_lossy().into_owned();
        let mut config = crate::runtime::always_on::AlwaysOnConfig::default_config();
        config.projects = vec![(project_key, true)];
        let manager = crate::runtime::always_on::AlwaysOnManager::new(
            crate::runtime::always_on::AlwaysOnManagerOptions {
                config,
                pilot_home: requests.options.home.clone(),
                now: None,
                uuid: None,
                logger: None,
            },
        )
        .expect("#2294 manager constructs");
        struct NoStandalone;
        impl crate::runtime::always_on::StandaloneApply for NoStandalone {
            fn apply<'a>(
                &'a self,
                _: &'a JsString,
                _: JsValue,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<JsValue, crate::runtime::always_on::ControlError>,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async { unreachable!("configured manager uses live control") })
            }
        }
        let slot = std::sync::Arc::new(
            crate::native_process::assembly::automation::PublishedAutomation::default(),
        );
        slot.set_control_for_test(crate::runtime::always_on::create_control(
            Some(std::sync::Arc::new(manager)),
            std::sync::Arc::new(NoStandalone),
        ));
        requests.automation = Some(slot);
        requests
    }

    // The configured routing delegates to the control port verbatim: the
    // envelopes are the r1/r9 golden shapes — key order, omitted-undefined
    // and all — through the live WS dispatch. The r9 `project_not_found`
    // envelope for an arbitrary key is manager-level (its own replay in
    // runtime/always_on/manager.rs): this process's shared admission gate
    // refuses an unserved projectKey before the family runs, so the wire
    // reaches it only through a missing key, which the template renders as
    // `undefined`.
    #[tokio::test]
    async fn configured_manager_routes_the_three_methods() {
        let requests = configured_requests().await;
        let project = requests.options.project.to_string_lossy().into_owned();
        let full = |fields: Vec<(&'static str, JsValue)>| crate::metadata::object(fields);
        for (method, params, envelope) in [
            // A missing projectKey reads as `undefined` in the template.
            (
                "always_on_apply",
                full(vec![("workCycleId", "wc1".into())]),
                "{\"sessionKey\":\"\",\"error\":{\"code\":\"project_not_found\",\"message\":\
                 \"No Always-On runtime for project undefined\"}}",
            ),
            // r1: the configured project's unbound runtime answers
            // `not_ready` — the runtime-half envelopes, now at the wire.
            (
                "always_on_apply",
                full(vec![
                    ("projectKey", project.as_str().into()),
                    ("workCycleId", "c1".into()),
                ]),
                "{\"sessionKey\":\"\",\"error\":{\"code\":\"not_ready\",\"message\":\
                 \"AlwaysOnRuntime.bindGateway not called\"}}",
            ),
            (
                "always_on_abort",
                full(vec![
                    ("projectKey", project.as_str().into()),
                    ("sessionKey", "s1".into()),
                ]),
                "{\"aborted\":false,\"sessionKey\":\"s1\",\"error\":{\"code\":\"not_ready\",\
                 \"message\":\"AlwaysOnRuntime.bindGateway not called\"}}",
            ),
            // The abort echo is the RAW wire value, dropped when the key is
            // missing (JSON.stringify over `sessionKey: undefined`).
            (
                "always_on_abort",
                full(vec![
                    ("projectKey", project.as_str().into()),
                    ("sessionKey", JsValue::Number(42.0)),
                ]),
                "{\"aborted\":false,\"sessionKey\":42,\"error\":{\"code\":\"not_ready\",\
                 \"message\":\"AlwaysOnRuntime.bindGateway not called\"}}",
            ),
            (
                "always_on_abort",
                full(vec![("projectKey", project.as_str().into())]),
                "{\"aborted\":false,\"error\":{\"code\":\"not_ready\",\"message\":\
                 \"AlwaysOnRuntime.bindGateway not called\"}}",
            ),
            (
                "always_on_rerun_plan",
                full(vec![
                    ("projectKey", project.as_str().into()),
                    ("planId", "p1".into()),
                ]),
                "{\"runId\":\"\",\"error\":{\"code\":\"not_ready\",\"message\":\
                 \"AlwaysOnRuntime.bindGateway not called\"}}",
            ),
            (
                "always_on_rerun_plan",
                full(vec![("planId", "p1".into())]),
                "{\"runId\":\"\",\"error\":{\"code\":\"project_not_found\",\"message\":\
                 \"No Always-On runtime for project undefined\"}}",
            ),
        ] {
            let result = GatewayRequests::request(&requests, JsString::from(method), params).await;
            let value = result
                .unwrap_or_else(|error| panic!("#2294 {method} answers an envelope: {error:?}"));
            let text = stringify_json(&value)
                .expect("#2294 envelope serializes")
                .expect("#2294 envelope is defined");
            negative_assert_eq!(text, envelope, "#2294 {method} configured routing");
        }
        // A staged manager does not bypass the shared admission gate: an
        // unserved projectKey is still refused before the family runs.
        let params = crate::metadata::object([("projectKey", "/elsewhere".into())]);
        let result =
            GatewayRequests::request(&requests, JsString::from("always_on_apply"), params).await;
        negative_assert_eq!(
            result
                .expect_err("#2294 unserved project is refused with a manager too")
                .code
                .as_str(),
            "gateway_request_failed",
            "#2294 the admission gate precedes the always-on family"
        );
        teardown(&requests).await;
    }

    // With a manager present, every arm reads `input.projectKey` first (the
    // manager's runtime find — AlwaysOnManager.ts:185-215), so a nullish
    // `params` reports the projectKey read on all three methods — the reverse
    // of the unconfigured branch, where only abort reads `sessionKey`.
    #[tokio::test]
    async fn configured_nullish_params_hit_the_project_key_read() {
        let requests = configured_requests().await;
        for method in crate::runtime::always_on::ALWAYS_ON_METHODS {
            for (params, origin) in [(JsValue::Null, "null"), (JsValue::Undefined, "undefined")] {
                let result =
                    GatewayRequests::request(&requests, JsString::from(*method), params).await;
                let error = result.expect_err("#2294 configured arms read projectKey first");
                negative_assert_eq!(
                    error.code.as_str(),
                    "gateway_request_failed",
                    "#2294 {method} nullish {origin} code"
                );
                negative_assert_eq!(
                    error.message.to_utf8().expect("#2294 message UTF-8"),
                    format!("Cannot read properties of {origin} (reading 'projectKey')"),
                    "#2294 {method} nullish {origin} message"
                );
            }
        }
        teardown(&requests).await;
    }

    // The advertisement flips with the manager: the three names appear in
    // describe exactly when a staged manager exists — the `options.alwaysOnControl`
    // gate (InProcessGateway.ts:2123-2126).
    #[tokio::test]
    async fn a_staged_manager_advertises_the_three_names() {
        let requests = configured_requests().await;
        let described = GatewayRequests::describe_server(&requests)
            .await
            .expect("#2294 describe");
        let capabilities = match described.get("capabilities") {
            Some(pilotdeck_protocol::JsValue::Array(items)) => items,
            _ => panic!("#2294 capability array"),
        };
        for method in crate::runtime::always_on::ALWAYS_ON_METHODS {
            negative_assert!(
                capabilities.iter().any(|capability| {
                    capability
                        .as_string()
                        .is_some_and(|name| name.equals_str(method))
                }),
                "#2294 the configured runtime advertises {method}"
            );
        }
        teardown(&requests).await;
    }
}
