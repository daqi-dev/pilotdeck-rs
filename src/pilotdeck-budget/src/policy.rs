//! Pure local budget snapshot policy from TokenBudgetManager.
use pilotdeck_protocol::js_math::max as js_max;
use pilotdeck_protocol::{JsObject, JsValue};
#[derive(Clone, Debug)]
pub struct TokenBudgetPolicy {
    pub warning_ratio: f64,
    pub blocking_ratio: f64,
}
impl Default for TokenBudgetPolicy {
    fn default() -> Self {
        Self {
            warning_ratio: 0.8,
            blocking_ratio: 0.9,
        }
    }
}
impl TokenBudgetPolicy {
    pub fn snapshot_from_tokens(&self, tokens: f64, context: f64, metadata: &JsObject) -> JsValue {
        let reserved = js_max(
            metadata
                .get("reservedOutputTokens")
                .and_then(JsValue::as_number)
                .unwrap_or(0.0)
                .floor(),
            0.0,
        );
        let total = js_max(context.floor(), 1.0);
        let budget = if context.is_finite() && context > 0.0 {
            js_max(
                context.floor() - if reserved.is_finite() { reserved } else { 0.0 },
                1.0,
            )
        } else {
            context
        };
        let ratio = if budget > 0.0 { tokens / budget } else { 0.0 };
        let state = if ratio >= self.blocking_ratio {
            "blocking"
        } else if ratio >= self.warning_ratio {
            "warning"
        } else {
            "ok"
        };
        let mut result = JsObject::new();
        result.insert("tokens".into(), JsValue::Number(tokens));
        for key in ["localEstimateTokens", "displayTokens"] {
            if let Some(value) = metadata.get(key).filter(|v| !v.is_undefined())
                && (key != "displayTokens" || value.as_number() != Some(tokens))
            {
                result.insert(key.into(), value.clone());
            }
        }
        let usage = metadata
            .get("usageTokens")
            .is_some_and(|v| !v.is_undefined());
        result.insert(
            "estimateSource".into(),
            if usage { "usage" } else { "estimator" }.into(),
        );
        for key in [
            "usageTokens",
            "calibrationActualInputTokens",
            "calibrationEstimatedInputTokens",
            "breakdown",
        ] {
            if let Some(value) = metadata.get(key).filter(|v| !v.is_undefined()) {
                result.insert(key.into(), value.clone());
            }
        }
        for (key, number) in [
            ("totalContextTokens", total),
            ("maxContextTokens", budget),
            ("effectiveContextTokens", budget),
            ("maxOutputTokens", reserved),
            ("warningRatio", self.warning_ratio),
            ("blockingRatio", self.blocking_ratio),
        ] {
            result.insert(key.into(), JsValue::Number(number));
        }
        result.insert("state".into(), state.into());
        result.insert("ratio".into(), JsValue::Number(ratio));
        for key in ["source", "exact"] {
            if let Some(value) = metadata.get(key) {
                result.insert(key.into(), value.clone());
            }
        }
        result.insert("reservedOutputTokens".into(), JsValue::Number(reserved));
        if let Some(value) = metadata.get("estimatorError") {
            result.insert("estimatorError".into(), value.clone());
        }
        JsValue::Object(result)
    }
}
