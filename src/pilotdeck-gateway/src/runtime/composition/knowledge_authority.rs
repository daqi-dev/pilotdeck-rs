//! TS `sopCapabilityAuthority.ts:24-28` and `SopAgentLoop.ts:748-749`.
use pilotdeck_protocol::{JsObject, JsValue};

/// Builds the execution context written into module HTTP `authorityContext`.
/// Production callers set `knowledgeAuthority` on the turn input, or
/// `sopOptionalKnowledge` (StaffDeck SOP projection) which this helper merges.
pub fn knowledge_authority_from_turn(
    turn_input: &JsObject,
    call_context: Option<&JsObject>,
) -> Option<JsObject> {
    if let Some(authority) = turn_input
        .get("knowledgeAuthority")
        .and_then(JsValue::as_object)
    {
        return Some(authority.clone());
    }
    if let Some(projection) = turn_input
        .get("sopOptionalKnowledge")
        .and_then(JsValue::as_object)
    {
        return knowledge_authority_from_optional_knowledge(projection);
    }
    if let Some(projection) = call_context
        .and_then(|value| {
            value
                .get("optionalKnowledge")
                .or_else(|| value.get("sopOptionalKnowledge"))
        })
        .and_then(JsValue::as_object)
    {
        return knowledge_authority_from_optional_knowledge(projection);
    }
    call_context
        .and_then(|value| value.get("knowledgeAuthority"))
        .and_then(JsValue::as_object)
        .cloned()
}

/// TS `SopAgentLoop.ts:748-749` projection → authority object shape.
pub fn knowledge_authority_from_optional_knowledge(projection: &JsObject) -> Option<JsObject> {
    let mut authority = projection
        .get("context")
        .and_then(JsValue::as_object)
        .cloned()
        .unwrap_or_default();
    for key in ["snapshotId", "registryGeneration"] {
        if let Some(value) = projection.get(key) {
            authority.insert(key.into(), value.clone());
        }
    }
    if authority.is_empty() {
        None
    } else {
        Some(authority)
    }
}

pub fn merge_knowledge_authority_into_fields(
    fields: &mut JsObject,
    turn_input: &JsObject,
    call_context: Option<&JsObject>,
) {
    if let Some(authority) = knowledge_authority_from_turn(turn_input, call_context) {
        fields.insert("knowledgeAuthority".into(), JsValue::Object(authority));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{JsObject, JsString, JsValue};

    fn s(value: &str) -> JsValue {
        JsValue::String(JsString::from(value))
    }

    #[test]
    fn sop_optional_knowledge_projection_matches_sop_agent_loop_shape() {
        let mut context = JsObject::new();
        context.insert("sessionKey".into(), s("sess"));
        context.insert("projectKey".into(), s("proj"));
        context.insert("expectedRevision".into(), JsValue::Number(2.0));
        context.insert("requestId".into(), s("req-1"));
        let mut projection = JsObject::new();
        projection.insert("context".into(), JsValue::Object(context));
        projection.insert("snapshotId".into(), s("snap-a"));
        projection.insert("registryGeneration".into(), JsValue::Number(7.0));
        let mut turn = JsObject::new();
        turn.insert("sopOptionalKnowledge".into(), JsValue::Object(projection));
        let authority = knowledge_authority_from_turn(&turn, None).expect("built");
        assert_eq!(authority.get("sessionKey"), Some(&s("sess")));
        assert_eq!(authority.get("snapshotId"), Some(&s("snap-a")));
        assert_eq!(
            authority.get("registryGeneration"),
            Some(&JsValue::Number(7.0))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_authority_survives_yield_on_worker_pool() {
        let mut authority = JsObject::new();
        authority.insert("sessionKey".into(), s("s1"));
        authority.insert("snapshotId".into(), s("snap"));
        authority.insert("registryGeneration".into(), JsValue::Number(1.0));
        let captured = tokio::spawn(async move {
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            authority.clone()
        })
        .await
        .expect("join");
        assert_eq!(
            captured.get("snapshotId"),
            Some(&s("snap")),
            "explicit authority must survive async yield on the worker pool"
        );
    }
}
