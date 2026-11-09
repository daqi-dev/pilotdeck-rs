//! The model-only part of the committed SDK session config.
//! Mirrors SessionAgentConfigBundle.resolveFallbackModel; values remain UTF-16.
use crate::runtime::model::router::{ManagedModelPolicy, RouterModelRef};
use pilotdeck_protocol::{JsString, JsValue};

use crate::runtime::turn::interaction::SdkModelOptions;

pub(crate) fn model_options(
    config: &JsValue,
    agent: Option<RouterModelRef>,
    enforced: Option<&JsValue>,
) -> SdkModelOptions {
    let selectors = |key: &str| match config.get("managedModels").and_then(|v| v.get(key)) {
        Some(JsValue::Array(items)) => items
            .iter()
            .filter_map(JsValue::as_string)
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    let policy = config
        .get("managedModels")
        .and_then(JsValue::as_object)
        .map(|_| ManagedModelPolicy {
            allow: selectors("allow"),
            deny: selectors("deny"),
        });
    let sdk_fallback = config
        .get("fallbackModel")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .or_else(|| {
            config
                .get("settings")
                .and_then(|v| v.get("agent"))
                .and_then(|v| v.get("fallbackModel"))
        });
    let fallback = enforced
        .filter(|v| !v.is_undefined())
        .or(sdk_fallback)
        .and_then(JsValue::as_string)
        .and_then(|value| {
            let units = value.units();
            match units.iter().position(|c| *c == u16::from(b'/')) {
                Some(i) if i > 0 && i + 1 < units.len() => Some(RouterModelRef::new(
                    JsString::from_utf16(units[..i].to_vec()),
                    JsString::from_utf16(units[i + 1..].to_vec()),
                )),
                _ => agent,
            }
        })
        .into_iter()
        .collect();
    SdkModelOptions { policy, fallback }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::object;
    #[test]
    fn fallback_precedence_and_utf16_follow_the_session_bundle() {
        let model = RouterModelRef::new("p".into(), "base".into());
        let settings = object([("agent", object([("fallbackModel", "p/settings".into())]))]);
        let config = object([
            ("fallbackModel", "other/model".into()),
            ("settings", settings),
        ]);
        let options = model_options(&config, Some(model.clone()), None);
        assert_eq!(
            options.fallback[0],
            RouterModelRef::new("other".into(), "model".into())
        );
        assert!(
            model_options(&config, Some(model.clone()), Some(&JsValue::Null))
                .fallback
                .is_empty()
        );
        let short = object([("fallbackModel", "short".into())]);
        assert_eq!(
            model_options(&short, Some(model.clone()), None).fallback,
            vec![model]
        );
        let raw = JsString::from_utf16(vec![0xd800, 47, 0xdc00]);
        let opts = model_options(
            &object([("fallbackModel", JsValue::String(raw))]),
            None,
            None,
        );
        assert_eq!(opts.fallback[0].provider.units(), &[0xd800]);
        assert_eq!(opts.fallback[0].model.units(), &[0xdc00]);
    }
    #[test]
    fn fixed_ts_session_bundle_model_options_match() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/native-sdk-model/oracle.json"
        ))
        .unwrap();
        let Some(JsValue::Array(records)) = oracle.get("records") else {
            panic!("oracle records")
        };
        assert_eq!(records.len(), 8);
        for record in records {
            let id = record.get("id").and_then(JsValue::as_string).unwrap();
            let sdk = record.get("sdk").unwrap();
            let enforced = if id.equals_str("enforced-null") || id.equals_str("enforced-exact") {
                record.get("enforced")
            } else {
                None
            };
            let model = RouterModelRef::new("fixture".into(), "main".into());
            let actual = model_options(sdk, Some(model), enforced);
            let expected = record.get("result").expect("TS case must not error");
            let expected_fallback = match expected.get("fallbackModels") {
                Some(JsValue::Array(items)) => items
                    .iter()
                    .map(|item| {
                        RouterModelRef::new(
                            item.get("provider")
                                .and_then(JsValue::as_string)
                                .unwrap()
                                .clone(),
                            item.get("model")
                                .and_then(JsValue::as_string)
                                .unwrap()
                                .clone(),
                        )
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            assert_eq!(actual.fallback, expected_fallback, "fallback {id:?}");
            let expected_policy = expected
                .get("managedModelPolicy")
                .and_then(JsValue::as_object)
                .map(|p| {
                    let items = |key| match p.get(key) {
                        Some(JsValue::Array(a)) => {
                            a.iter().map(|v| v.as_string().unwrap().clone()).collect()
                        }
                        _ => Vec::new(),
                    };
                    ManagedModelPolicy {
                        allow: items("allow"),
                        deny: items("deny"),
                    }
                });
            assert_eq!(actual.policy, expected_policy, "policy {id:?}");
        }
    }
}
