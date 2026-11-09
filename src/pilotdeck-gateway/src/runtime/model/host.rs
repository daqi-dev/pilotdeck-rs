//! Snapshot-backed model metadata using the daqi host's raw JS-value boundary.
use crate::host::{ModuleError, modules::model::ModelMetadata};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub struct SnapshotMetadata(pub JsValue);

fn key(value: &JsValue) -> Result<JsString, ModuleError> {
    crate::js_value::to_string_units_checked(value)
        .map(JsString::from_utf16)
        .map_err(|error| ModuleError::new(error.message()))
}

impl SnapshotMetadata {
    fn provider(&self, provider: &JsValue) -> Result<Option<&JsValue>, ModuleError> {
        let key = key(provider)?;
        Ok(self
            .0
            .get("config")
            .and_then(|config| config.get("model"))
            .and_then(|model| model.get("providers"))
            .and_then(|providers| providers.as_object()?.get_key(&key)))
    }

    fn capabilities(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<&JsValue>, ModuleError> {
        let Some(provider) = self.provider(provider)? else {
            return Ok(None);
        };
        let Some(models) = provider.get("models").and_then(JsValue::as_object) else {
            return Ok(None);
        };
        let model = key(model)?;
        Ok(models
            .get_key(&model)
            .and_then(|model| model.get("capabilities")))
    }
}

impl ModelMetadata for SnapshotMetadata {
    fn get_model_max_context_tokens(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(self
            .capabilities(provider, model)?
            .and_then(|value| value.get("maxContextTokens"))
            .cloned())
    }

    fn get_model_max_output_tokens(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(self
            .capabilities(provider, model)?
            .and_then(|value| value.get("maxOutputTokens"))
            .cloned())
    }

    fn get_model_token_limits(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        let Some(capabilities) = self.capabilities(provider, model)? else {
            return Ok(None);
        };
        let mut limits = JsObject::new();
        for name in ["maxContextTokens", "maxOutputTokens"] {
            if let Some(value) = capabilities.get(name) {
                limits.insert(name.into(), value.clone());
            }
        }
        Ok(Some(JsValue::Object(limits)))
    }

    fn get_model_protocol(&self, provider: &JsValue) -> Result<Option<JsValue>, ModuleError> {
        Ok(self
            .provider(provider)?
            .and_then(|value| value.get("protocol"))
            .cloned())
    }

    fn get_model_supports_prompt_cache(
        &self,
        provider: &JsValue,
        model: &JsValue,
    ) -> Result<Option<JsValue>, ModuleError> {
        Ok(self
            .capabilities(provider, model)?
            .and_then(|value| value.get("supportsPromptCache"))
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::object;

    #[test]
    fn raw_route_values_index_the_snapshot_with_js_string_conversion() {
        let capabilities = object([
            ("maxContextTokens", JsValue::Number(8192.0)),
            ("maxOutputTokens", JsValue::Number(1024.0)),
            ("supportsPromptCache", JsValue::Bool(true)),
        ]);
        let metadata = SnapshotMetadata(object([(
            "config",
            object([(
                "model",
                object([(
                    "providers",
                    object([(
                        "7",
                        object([
                            ("protocol", "openai".into()),
                            (
                                "models",
                                object([("true", object([("capabilities", capabilities)]))]),
                            ),
                        ]),
                    )]),
                )]),
            )]),
        )]));
        let provider = JsValue::Number(7.0);
        let model = JsValue::Bool(true);
        assert_eq!(
            metadata
                .get_model_max_context_tokens(&provider, &model)
                .unwrap(),
            Some(JsValue::Number(8192.0))
        );
        assert_eq!(
            metadata
                .get_model_max_output_tokens(&provider, &model)
                .unwrap(),
            Some(JsValue::Number(1024.0))
        );
        assert_eq!(
            metadata.get_model_token_limits(&provider, &model).unwrap(),
            Some(object([
                ("maxContextTokens", JsValue::Number(8192.0)),
                ("maxOutputTokens", JsValue::Number(1024.0)),
            ]))
        );
        assert_eq!(
            metadata.get_model_protocol(&provider).unwrap(),
            Some("openai".into())
        );
        assert_eq!(
            metadata
                .get_model_supports_prompt_cache(&provider, &model)
                .unwrap(),
            Some(JsValue::Bool(true))
        );
        assert_eq!(
            metadata.get_model_protocol(&"missing".into()).unwrap(),
            None
        );
        assert!(
            metadata
                .get_model_protocol(&object([("toString", JsValue::Null)]))
                .is_err()
        );
    }
}
