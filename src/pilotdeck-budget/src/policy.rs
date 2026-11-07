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

impl TokenBudgetPolicy {
    /// Local branch of TokenAccountingRuntime with fixed-baseline additive,
    /// route-scoped calibration (bounded to half the local estimate).
    pub fn evaluate_local_request(
        &self,
        raw: &JsObject,
        context: f64,
        reserved: Option<f64>,
        calibration: Option<&JsObject>,
    ) -> Result<JsValue, crate::wire::AccountingError> {
        let (request, extra) = crate::wire::accounting_request(raw)?;
        let counts = crate::estimate_request_breakdown(&request)
            .map_err(|e| crate::wire::AccountingError::new(format!("{e:?}")))?;
        let local = (counts.total + extra) as f64;
        let mut tokens = local;
        let mut metadata = JsObject::new();
        let mut source = "local";
        if let Some(calibration) =
            calibration.filter(|c| ["provider", "model"].iter().all(|k| c.get(k) == raw.get(k)))
        {
            let positive = |k: &str| {
                calibration
                    .get(k)
                    .and_then(JsValue::as_number)
                    .filter(|n| n.is_finite() && *n > 0.0)
            };
            if let (Some(actual), Some(estimated)) = (
                positive("actualInputTokens"),
                positive("estimatedInputTokens"),
            ) {
                let maximum = js_max((local * 0.5).floor(), 1.0);
                tokens = js_max(
                    (local
                        + pilotdeck_protocol::js_math::min(
                            maximum,
                            js_max(-maximum, actual - estimated),
                        )
                        + 0.5)
                        .floor(),
                    1.0,
                );
                source = "calibrated";
                metadata.insert(
                    "calibrationActualInputTokens".into(),
                    JsValue::Number(actual),
                );
                metadata.insert(
                    "calibrationEstimatedInputTokens".into(),
                    JsValue::Number(estimated),
                );
                metadata.insert("usageTokens".into(), JsValue::Number(tokens));
            }
        }
        let mut breakdown = JsObject::new();
        breakdown.insert("source".into(), "local_estimate".into());
        for (k, n) in [
            ("total", counts.total + extra),
            ("system", counts.system),
            ("tools", counts.tools + extra),
            ("messages", counts.messages),
            ("mcp", counts.mcp),
            ("memory", counts.memory),
        ] {
            breakdown.insert(k.into(), JsValue::Number(n as f64));
        }
        metadata.insert("breakdown".into(), JsValue::Object(breakdown));
        metadata.insert("localEstimateTokens".into(), JsValue::Number(local));
        metadata.insert("source".into(), source.into());
        metadata.insert("exact".into(), JsValue::Bool(false));
        if let Some(reserved) = reserved {
            metadata.insert("reservedOutputTokens".into(), JsValue::Number(reserved));
        }
        Ok(self.snapshot_from_tokens(tokens, context, &metadata))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn calibration_is_additive_bounded_and_route_scoped() {
        let request=parse_json(r#"{"provider":"p","model":"m","messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}]}"#).unwrap();
        let request = request.as_object().unwrap();
        let calibration = parse_json(
            r#"{"provider":"p","model":"m","actualInputTokens":100000,"estimatedInputTokens":1}"#,
        )
        .unwrap();
        let result = TokenBudgetPolicy::default()
            .evaluate_local_request(request, 8000.0, None, calibration.as_object())
            .unwrap();
        let local = result
            .get("localEstimateTokens")
            .and_then(JsValue::as_number)
            .unwrap();
        assert_eq!(
            result.get("tokens").and_then(JsValue::as_number),
            Some(local + (local * 0.5).floor().max(1.0))
        );
        let wrong=parse_json(r#"{"provider":"other","model":"m","actualInputTokens":100000,"estimatedInputTokens":1}"#).unwrap();
        let result = TokenBudgetPolicy::default()
            .evaluate_local_request(request, 8000.0, None, wrong.as_object())
            .unwrap();
        assert_eq!(result.get("source"), Some(&JsValue::from("local")));
    }
}
