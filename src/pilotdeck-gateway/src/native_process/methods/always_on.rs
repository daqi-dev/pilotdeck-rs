use super::super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::JsValue;

// Wire names are the WebSocket frame method names from the TS frames registry.
// 2026-11-05 human decision (state/decisions-20261105-1313-rescope.md, option
// A): always-on is deferred and the three methods answer honestly
// CAPABILITY_UNAVAILABLE — no false capability claim, no TS fallback, no
// silence. TS returns not_configured success envelopes
// (InProcessGateway.ts:2991-3013); that envelope-level divergence and its
// registration belong to #1476, so this PR keeps the TS message text but uses
// the capability-unavailable code.
pub(super) const METHODS: &[&str] = &["always_on_apply", "always_on_abort", "always_on_rerun_plan"];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::always_on_request);

fn unavailable(message: &str) -> RequestError {
    RequestError {
        code: "CAPABILITY_UNAVAILABLE".into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

impl Requests {
    pub(in crate::native_process) fn always_on_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            let _ = params;
            let message = match method.as_str() {
                "always_on_abort" => "Always-On abort is not configured on this gateway.",
                "always_on_rerun_plan" => "Always-On rerun is not configured on this gateway.",
                _ => "Always-On apply is not configured on this gateway.",
            };
            // Like the SOP family, the capability gate precedes every params
            // guard: TS has no runtime schema parsing at the connection layer
            // and the not-configured check is the first statement of each
            // alwaysOn* method (InProcessGateway.ts:2991-3013), so any field
            // access downstream is never reached.
            Err(unavailable(message))
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::turn::gateway::GatewayRequests;
    use crate::{native_process::management_tests::setup, negative_assert_eq};
    use pilotdeck_protocol::JsString;

    async fn teardown(requests: &crate::native_process::requests::Requests) {
        requests.owner.shutdown().await.expect("#1316 shutdown");
        requests
            .project
            .dispose()
            .await
            .expect("#1316 project dispose");
    }

    // The 2026-11-05 rescope requires the three deferred always-on methods to
    // answer CAPABILITY_UNAVAILABLE on every input, before any params guard.
    #[tokio::test]
    async fn always_on_methods_answer_capability_unavailable() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        for (method, message) in [
            (
                "always_on_apply",
                "Always-On apply is not configured on this gateway.",
            ),
            (
                "always_on_abort",
                "Always-On abort is not configured on this gateway.",
            ),
            (
                "always_on_rerun_plan",
                "Always-On rerun is not configured on this gateway.",
            ),
        ] {
            // Empty params exercise the gate-before-validation order.
            let result = GatewayRequests::request(
                &requests,
                JsString::from(method),
                pilotdeck_protocol::JsValue::Object(Default::default()),
            )
            .await;
            let error = result.expect_err("#1316 always-on unavailable");
            negative_assert_eq!(
                error.code.as_str(),
                "CAPABILITY_UNAVAILABLE",
                "#1316 {method} code"
            );
            negative_assert_eq!(
                error.message.to_utf8().expect("#1316 message UTF-8"),
                message,
                "#1316 {method} message"
            );
        }
        teardown(&requests).await;
    }

    // The deferred methods must not be advertised in describe: an honest
    // capability list contains only genuinely served methods.
    #[tokio::test]
    async fn always_on_methods_are_not_advertised() {
        let (_scratch, requests) = std::panic::catch_unwind(setup).expect("#1316 fixture setup");
        let described = GatewayRequests::describe_server(&requests)
            .await
            .expect("#1316 describe");
        let capabilities = match described.get("capabilities") {
            Some(pilotdeck_protocol::JsValue::Array(items)) => items,
            _ => panic!("#1316 capability array"),
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
                "#1316 {method} must not be advertised in describe"
            );
        }
        teardown(&requests).await;
    }
}
