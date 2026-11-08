//! Shared thinking request policy, with gateway-owned error projection.
use crate::runtime::model::http::ModelStreamError;
pub(crate) use pilotdeck_model_anthropic::thinking::ThinkingPlan;
use pilotdeck_protocol::JsValue;
pub(crate) fn resolve(
    request: &JsValue,
    provider: &JsValue,
    model: &JsValue,
) -> Result<ThinkingPlan, ModelStreamError> {
    pilotdeck_model_anthropic::thinking::resolve(request, provider, model)
        .map_err(super::anthropic::convert)
}
pub(crate) fn throw_if_unsupported(
    plan: &ThinkingPlan,
    request: &JsValue,
) -> Result<(), ModelStreamError> {
    pilotdeck_model_anthropic::thinking::throw_if_unsupported(plan, request)
        .map_err(super::anthropic::convert)
}
#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    #[test]
    fn unsupported_details_keep_the_request_and_undefined_mode() {
        let request = parse_json(r#"{"provider":"custom","model":"alias"}"#).unwrap();
        let plan = ThinkingPlan {
            unsupported_reason: Some("unsupported".into()),
            ..ThinkingPlan::default()
        };
        let ModelStreamError::Request {
            details: Some(details),
            ..
        } = throw_if_unsupported(&plan, &request).unwrap_err()
        else {
            panic!("expected thinking request details");
        };
        assert_eq!(details.get("provider"), request.get("provider"));
        assert_eq!(details.get("model"), request.get("model"));
        assert_eq!(details.get("thinkingMode"), Some(&JsValue::Undefined));
    }

    fn resolve_model(model_json: &str) -> Result<ThinkingPlan, ModelStreamError> {
        let request = parse_json(r#"{"thinking":{"mode":"high"}}"#).unwrap();
        let provider = parse_json(r#"{"id":"openai-fixture","protocol":"openai"}"#).unwrap();
        let model = parse_json(model_json).unwrap();
        resolve(&request, &provider, &model)
    }
    fn message(error: &ModelStreamError) -> String {
        String::from_utf16_lossy(error.message().units())
    }

    /// node probe (`fix1-probe-thinking.mts`): `settings?.efforts.includes`
    /// distinguishes null from undefined, accepts a string by substring, and
    /// reports a non-string receiver as "not a function".
    #[test]
    fn efforts_edge_types_match_the_typescript_receiver() {
        let null = resolve_model(
            r#"{"id":"m","thinking":{"state":"enabled","efforts":null,"format":"provider"}}"#,
        )
        .unwrap_err();
        assert_eq!(
            message(&null),
            "Cannot read properties of null (reading 'includes')"
        );
        let undefined =
            resolve_model(r#"{"id":"m","thinking":{"state":"enabled","format":"provider"}}"#)
                .unwrap_err();
        assert_eq!(
            message(&undefined),
            "Cannot read properties of undefined (reading 'includes')"
        );
        let number = resolve_model(
            r#"{"id":"m","thinking":{"state":"enabled","efforts":3,"format":"provider"}}"#,
        )
        .unwrap_err();
        assert_eq!(
            message(&number),
            "settings?.efforts.includes is not a function"
        );
        let substring = resolve_model(
            r#"{"id":"m","thinking":{"state":"enabled","efforts":"xx-high-xx","format":"provider"}}"#,
        )
        .unwrap();
        assert_eq!(
            substring.effort.as_ref().unwrap().to_utf8().unwrap(),
            "high"
        );
    }

    /// node probe (pr7 `r1/probe-r1.out.txt`): moonshot + kimi-k3 with an
    /// explicit `format:"provider"` patches only `reasoning_effort`; disabled
    /// is unsupported.
    #[test]
    fn always_thinking_endpoints_patch_only_the_effort() {
        let request = parse_json(r#"{"thinking":{"mode":"high"}}"#).unwrap();
        let provider = parse_json(r#"{"id":"moonshot","protocol":"openai"}"#).unwrap();
        let enabled = parse_json(r#"{"id":"kimi-k3","thinking":{"state":"enabled","efforts":["high"],"format":"provider"}}"#).unwrap();
        let plan = resolve(&request, &provider, &enabled).unwrap();
        assert_eq!(
            pilotdeck_protocol::stringify_json(plan.body_patch.as_ref().unwrap())
                .unwrap()
                .unwrap(),
            r#"{"reasoning_effort":"high"}"#
        );
        let disabled = parse_json(
            r#"{"id":"kimi-k3","thinking":{"state":"disabled","efforts":[],"format":"provider"}}"#,
        )
        .unwrap();
        let plan = resolve(&request, &provider, &disabled).unwrap();
        assert_eq!(
            plan.unsupported_reason.unwrap().to_utf8().unwrap(),
            "kimi-k3: This model cannot disable thinking. Uncheck both options to use the server default."
        );
    }
}
