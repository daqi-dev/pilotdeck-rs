use super::super::requests::Requests;
use crate::{
    metadata::object,
    runtime::{
        always_on::{AbortInput, ApplyInput, ControlError, RerunInput},
        connection::{Call, RequestError},
    },
};
use pilotdeck_protocol::JsValue;

// Wire names are the WebSocket frame method names from the TS frames registry.
// The 2026-11-05 #1475 deferral ("answer CAPABILITY_UNAVAILABLE honestly") was
// lifted on 2026-11-08, so the three methods now carry the TS "local gateway
// without an injected always-on control" behavior: a resolved result envelope
// whose `error` field carries `code: "not_configured"`
// (InProcessGateway.ts:2992-3016), which the WS boundary sends as `ok: true`
// (GatewayWsConnection.ts:186-187) — not a request-level error. #2294 lands
// the control port itself: a staged always-on manager (the assembly's
// automation bundle published one, `Requests::always_on`) routes to the real
// implementation, and no manager keeps the unconfigured branch below byte for
// byte.
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

/// The `error` field of a control result — the same key order
/// (`{code, message}`, AlwaysOnControlPort.ts).
fn control_error(error: &ControlError) -> JsValue {
    object([
        ("code", error.code.as_str().into()),
        ("message", error.message.as_str().into()),
    ])
}

/// JS `String(value)` rendering for interpolated control fields — the
/// `js_display` shape `methods/config.rs` uses.
fn js_display(value: &JsValue) -> String {
    String::from_utf16_lossy(&crate::js_value::to_string_units(value).unwrap_or_default())
}

/// `String(input.<key>)` — the coercion the TS control port's reads perform.
/// The wire `params` reach the port untouched, so a missing key reads as
/// `undefined`: the strict-equality project match and the
/// `No Always-On runtime for project ${...}` template both see its `String()`
/// form, `"undefined"` included. A non-string value also never matches a
/// runtime in TS; the port's coerced form can only collide with a project key
/// that is itself that decimal rendering, which the config's path-shaped keys
/// never are — recorded with the -R1 ledger.
fn wire_string(params: &JsValue, key: &str) -> Result<String, RequestError> {
    Ok(js_display(
        &super::input_property(params, key)?.unwrap_or(JsValue::Undefined),
    ))
}

/// An optional input field the control port carries through: `undefined` and
/// a missing key are `None`, anything else is its `String()` form.
fn wire_option(params: &JsValue, key: &str) -> Option<String> {
    params
        .get(key)
        .filter(|value| !value.is_undefined())
        .map(js_display)
}

/// A control-port rejection (`Promise` → throw) at the WS boundary: the same
/// `gateway_request_failed` catch-all the TypeError arms produce.
fn io_request_error(error: std::io::Error) -> RequestError {
    RequestError::new(error.to_string())
}

impl Requests {
    pub(in crate::native_process) fn always_on_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            // The whole configuration axis is one switch: TS gates on
            // `options.alwaysOnControl` (InProcessGateway.ts:2992-3016). A
            // staged manager delegates to the control port verbatim; no
            // manager answers the #1313 envelopes.
            let Some(manager) = self.always_on.clone() else {
                return not_configured(&method, &params);
            };
            match method.as_str() {
                // The TS reads happen in this order: the manager's runtime
                // find reads `projectKey` first, the cycle lookup reads
                // `workCycleId`, and `projectName` rides through to the fire
                // seam (AlwaysOnManager.ts:193-203).
                "always_on_apply" => {
                    let input = ApplyInput {
                        project_key: wire_string(&params, "projectKey")?,
                        work_cycle_id: wire_string(&params, "workCycleId")?,
                        project_name: wire_option(&params, "projectName"),
                    };
                    let result = manager
                        .apply_cycle(&input)
                        .await
                        .map_err(io_request_error)?;
                    let mut envelope = vec![("sessionKey", result.session_key.as_str().into())];
                    if let Some(error) = &result.error {
                        envelope.push(("error", control_error(error)));
                    }
                    Ok(object(envelope))
                }
                // The manager reads `projectKey` before `sessionKey` (the
                // runtime find first, the echo second — AlwaysOnManager.ts:
                // 205-215), so a nullish `params` reports the projectKey read:
                // the reverse of the unconfigured branch, which only reads
                // `sessionKey`. The envelope's `sessionKey` is the RAW wire
                // value like TS returns it — every control result echoes
                // `input.sessionKey` verbatim, and JSON.stringify drops an
                // `undefined` key.
                "always_on_abort" => {
                    let project_key = wire_string(&params, "projectKey")?;
                    let session_key = super::input_property(&params, "sessionKey")?;
                    let input = AbortInput {
                        project_key,
                        session_key: wire_string(&params, "sessionKey")?,
                        reason: wire_option(&params, "reason"),
                    };
                    let result = manager.abort_run(&input).await;
                    let mut envelope = vec![("aborted", JsValue::Bool(result.aborted))];
                    if let Some(value) = session_key.filter(|value| !value.is_undefined()) {
                        envelope.push(("sessionKey", value));
                    }
                    if let Some(run_id) = &result.run_id {
                        envelope.push(("runId", run_id.as_str().into()));
                    }
                    if let Some(error) = &result.error {
                        envelope.push(("error", control_error(error)));
                    }
                    Ok(object(envelope))
                }
                // The manager's find reads `projectKey`, the runtime then
                // reads `planId` (AlwaysOnManager.ts:185-191).
                "always_on_rerun_plan" => {
                    let input = RerunInput {
                        project_key: wire_string(&params, "projectKey")?,
                        plan_id: wire_string(&params, "planId")?,
                    };
                    let result = manager.rerun_plan(&input).await.map_err(io_request_error)?;
                    let mut envelope = vec![("runId", result.run_id.as_str().into())];
                    if let Some(error) = &result.error {
                        envelope.push(("error", control_error(error)));
                    }
                    Ok(object(envelope))
                }
                _ => unreachable!("#2294 the registry routes only the three always-on names"),
            }
        })
    }
}

/// The TS "no injected always-on control" branch
/// (InProcessGateway.ts:2992-3016): resolved result envelopes whose `error`
/// field carries `code: "not_configured"`.
fn not_configured(method: &str, params: &JsValue) -> Result<JsValue, RequestError> {
    match method {
        // The only arm that reads `input`: the unconfigured abort echoes
        // `input.sessionKey` in its return object (InProcessGateway.ts:3003),
        // so a nullish `params` throws the boundary TypeError —
        // `input_property` reproduces the message and `RequestError::new` the
        // `gateway_request_failed` wire code the TS catch-all produces
        // (GatewayWsConnection.ts:246-254). A present non-undefined value is
        // echoed verbatim (string, number and null alike), and a missing key
        // is dropped, matching JSON.stringify over `sessionKey: undefined`.
        "always_on_abort" => {
            let session_key = super::input_property(params, "sessionKey")?;
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
        // Neither remaining arm reads `input` in the unconfigured branch
        // (InProcessGateway.ts:2992-2997, :3010-3013), so any params shape —
        // including null and undefined — still answers the envelope.
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

    // The three names must stay out of describe without a manager: TS lists
    // them only when an `alwaysOnControl` is injected
    // (InProcessGateway.ts:2123-2126), and this fixture binds none.
    #[tokio::test]
    async fn always_on_methods_are_not_advertised_without_a_manager() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1313 fixture setup");
        let described = GatewayRequests::describe_server(&requests)
            .await
            .expect("#1313 describe");
        let capabilities = match described.get("capabilities") {
            Some(pilotdeck_protocol::JsValue::Array(items)) => items,
            _ => panic!("#1313 capability array"),
        };
        for method in crate::runtime::always_on::ALWAYS_ON_METHODS {
            let advertised = capabilities.iter().any(|capability| {
                capability
                    .as_string()
                    .is_some_and(|name| name.equals_str(method))
            });
            negative_assert_eq!(
                advertised,
                false,
                "#2294 {method} must not be advertised without an always-on manager"
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
        requests.always_on = Some(std::sync::Arc::new(manager));
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
