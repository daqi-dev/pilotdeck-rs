//! `thinking/registry.ts` (`resolveThinkingPlan`,
//! `throwIfUnsupportedThinkingPlan`) and `thinking/settings.ts`
//! (`THINKING_EFFORTS`), shared by the protocol request builders.
use super::super::count_body::{field, is, present, text};
use crate::runtime::model::http::ModelStreamError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::LazyLock;

const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

static GEMINI_3: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new("gemini-?3").expect("static regex"));

/// The fields of `ThinkingPlan` the ported request builders consume.
#[derive(Debug, Default)]
pub(crate) struct ThinkingPlan {
    /// `None` represents the default mode; otherwise an effort or `off`.
    pub(crate) mode: Option<JsString>,
    pub(crate) enabled: bool,
    pub(crate) thinking_level: Option<JsString>,
    pub(crate) body_patch: Option<JsValue>,
    pub(crate) thinking_type: Option<&'static str>,
    pub(crate) effort: Option<JsString>,
    pub(crate) use_anthropic_output_effort: bool,
    pub(crate) use_openai_reasoning: bool,
    pub(crate) unsupported_reason: Option<JsString>,
}

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in entries {
        if !value.is_undefined() {
            result.insert(key.into(), value);
        }
    }
    JsValue::Object(result)
}

fn protocol(provider: &JsValue) -> String {
    text(field(provider, "protocol"))
}

/// `resolveFormat(provider, format)`.
fn resolve_format(provider: &JsValue, format: &str) -> String {
    if format != "provider" {
        return format.to_string();
    }
    match protocol(provider).as_str() {
        "anthropic" => "anthropic".into(),
        "google" => "google".into(),
        "openai-responses" => "openai".into(),
        _ => match text(field(provider, "id")).as_str() {
            "openai" => "openai",
            "dashscope" => "qwen-cloud",
            "deepseek" | "moonshot" | "zhipu" => "thinking-type",
            "openrouter" => "openrouter",
            "ollama" | "minimax" | "volc_ark" => "server-default",
            _ => "openai",
        }
        .into(),
    }
}

fn unsupported(plan: &mut ThinkingPlan, model: &JsValue, message: &str) {
    let mut units = crate::js_value::to_string_units(field(model, "id")).unwrap_or_default();
    units.extend(": ".encode_utf16());
    units.extend(message.encode_utf16());
    plan.unsupported_reason = Some(JsString::from_utf16(units));
}

fn type_error(reading: &str) -> ModelStreamError {
    ModelStreamError::Thrown {
        name: "TypeError".into(),
        message: format!("Cannot read properties of undefined (reading '{reading}')").into(),
    }
}

/// `resolveThinkingPlan(request.thinking, provider, model)`.
pub(crate) fn resolve(
    request: &JsValue,
    provider: &JsValue,
    model: &JsValue,
) -> Result<ThinkingPlan, ModelStreamError> {
    let settings = field(model, "thinking");
    let state = field(settings, "state");
    if !present(state) || is(state, "default") {
        return Ok(ThinkingPlan::default());
    }
    let off = is(state, "disabled");
    let mode = field(field(request, "thinking"), "mode");
    let effort = (!off)
        .then(|| EFFORTS.iter().find(|effort| is(mode, effort)).copied())
        .flatten()
        .map(JsString::from);
    let mut plan = ThinkingPlan {
        mode: if off {
            Some("off".into())
        } else {
            effort.clone()
        },
        enabled: !off,
        ..ThinkingPlan::default()
    };
    if let Some(effort) = &effort {
        let effort_text = String::from_utf16_lossy(effort.units());
        let supported = match field(settings, "efforts") {
            JsValue::Array(efforts) => efforts.iter().any(|e| is(e, &effort_text)),
            JsValue::String(efforts) => {
                let needle: Vec<u16> = effort.units().to_vec();
                efforts.units().windows(needle.len()).any(|s| s == needle)
            }
            JsValue::Null => {
                return Err(ModelStreamError::Thrown {
                    name: "TypeError".into(),
                    message: "Cannot read properties of null (reading 'includes')".into(),
                });
            }
            JsValue::Undefined => {
                return Err(type_error("includes"));
            }
            _ => {
                return Err(ModelStreamError::Thrown {
                    name: "TypeError".into(),
                    message: "settings?.efforts.includes is not a function".into(),
                });
            }
        };
        if !supported {
            unsupported(
                &mut plan,
                model,
                &format!(
                    "Reasoning effort '{effort_text}' is not configured. Select Default or configure the model's supported efforts."
                ),
            );
            return Ok(plan);
        }
    }
    let raw_format = field(settings, "format");
    let format = if !present(raw_format) || is(raw_format, "provider") {
        "provider".to_string()
    } else {
        text(raw_format)
    };
    let format = resolve_format(provider, &format);
    if format == "server-default" {
        if off || effort.is_some() {
            unsupported(
                &mut plan,
                model,
                "This parameter format only supports the server default. Choose a thinking parameter format in model settings.",
            );
        }
        plan.enabled = false;
        return Ok(plan);
    }
    if format == "openai" {
        plan.effort = if off {
            Some("none".into())
        } else {
            effort.clone().filter(|effort| !effort.is_empty())
        };
        plan.use_openai_reasoning = true;
        return Ok(plan);
    }
    if format == "anthropic" {
        if off {
            plan.thinking_type = Some("disabled");
            return Ok(plan);
        }
        let model_id = text(field(model, "id"));
        // Shared with the count-tokens projection (pr5 P2-a).
        let budget_only = super::super::count_body::budget_only_model(&model_id);
        if budget_only {
            if effort.is_some() {
                unsupported(
                    &mut plan,
                    model,
                    "This model requires a thinking budget; only Default is supported.",
                );
            }
            plan.enabled = false;
            return Ok(plan);
        }
        plan.thinking_type = Some("adaptive");
        plan.effort = effort;
        plan.use_anthropic_output_effort = true;
        return Ok(plan);
    }
    let effort_patch = || -> Option<JsValue> {
        effort
            .as_ref()
            .map(|effort| object([("reasoning_effort", JsValue::String(effort.clone()))]))
    };
    if format == "google" {
        if off {
            unsupported(
                &mut plan,
                model,
                "Turning off thinking is not supported by this effort-only Gemini adapter. Uncheck both thinking options to use the server default.",
            );
            return Ok(plan);
        }
        if !GEMINI_3.is_match(&text(field(model, "id"))) {
            if effort.is_some() {
                unsupported(
                    &mut plan,
                    model,
                    "This Gemini model has no supported thinking-level adapter. Select Default.",
                );
            }
            plan.enabled = false;
            return Ok(plan);
        }
        if let Some(effort) = effort.as_ref().filter(|e| {
            let effort_text = String::from_utf16_lossy(e.units());
            !["low", "medium", "high"].contains(&effort_text.as_str())
        }) {
            unsupported(
                &mut plan,
                model,
                &format!(
                    "Gemini thinkingLevel does not support '{}'.",
                    String::from_utf16_lossy(effort.units())
                ),
            );
        } else {
            plan.thinking_level = effort;
        }
        return Ok(plan);
    }
    let provider_id = text(field(provider, "id"));
    let model_id = text(field(model, "id"));
    if is(raw_format, "provider")
        && ((provider_id == "moonshot" && model_id.starts_with("kimi-k3"))
            || (provider_id == "zhipu" && {
                let stripped = model_id.strip_prefix("glm-").unwrap_or(&model_id);
                if model_id.starts_with("glm-") {
                    stripped.starts_with("5.3")
                } else {
                    model_id.starts_with("glm5.3")
                }
            }))
    {
        if off {
            unsupported(
                &mut plan,
                model,
                "This model cannot disable thinking. Uncheck both options to use the server default.",
            );
            return Ok(plan);
        }
        plan.body_patch = effort_patch();
        return Ok(plan);
    }
    plan.body_patch = Some(match format.as_str() {
        "qwen-cloud" => {
            let mut patch = JsObject::new();
            patch.insert("enable_thinking".into(), JsValue::Bool(!off));
            if let Some(extra) = effort_patch().and_then(|v| v.as_object().cloned()) {
                for (key, value) in extra.entries() {
                    patch.insert(key.clone(), value.clone());
                }
            }
            JsValue::Object(patch)
        }
        "qwen-local" => {
            let mut patch = JsObject::new();
            patch.insert(
                "chat_template_kwargs".into(),
                object([("enable_thinking", JsValue::Bool(!off))]),
            );
            if let Some(extra) = effort_patch().and_then(|v| v.as_object().cloned()) {
                for (key, value) in extra.entries() {
                    patch.insert(key.clone(), value.clone());
                }
            }
            JsValue::Object(patch)
        }
        "openrouter" => {
            let mut reasoning = JsObject::new();
            if off {
                reasoning.insert("enabled".into(), JsValue::Bool(false));
            } else {
                reasoning.insert("enabled".into(), JsValue::Bool(true));
                if let Some(effort) = &effort {
                    reasoning.insert("effort".into(), JsValue::String(effort.clone()));
                }
            }
            object([("reasoning", JsValue::Object(reasoning))])
        }
        _ => {
            let mut patch = JsObject::new();
            patch.insert(
                "thinking".into(),
                object([("type", if off { "disabled" } else { "enabled" }.into())]),
            );
            if let Some(effort) = &effort {
                patch.insert("reasoning_effort".into(), JsValue::String(effort.clone()));
            }
            JsValue::Object(patch)
        }
    });
    Ok(plan)
}

/// `throwIfUnsupportedThinkingPlan(plan, request)`.
pub(crate) fn throw_if_unsupported(
    plan: &ThinkingPlan,
    request: &JsValue,
) -> Result<(), ModelStreamError> {
    match &plan.unsupported_reason {
        Some(reason) => {
            let mut details = JsObject::new();
            details.insert("provider".into(), field(request, "provider").clone());
            details.insert("model".into(), field(request, "model").clone());
            details.insert(
                "thinkingMode".into(),
                plan.mode
                    .clone()
                    .map(JsValue::String)
                    .unwrap_or(JsValue::Undefined),
            );
            Err(ModelStreamError::Request {
                code: "unsupported_thinking".into(),
                message: reason.clone(),
                details: Some(Box::new(JsValue::Object(details))),
            })
        }
        None => Ok(()),
    }
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
