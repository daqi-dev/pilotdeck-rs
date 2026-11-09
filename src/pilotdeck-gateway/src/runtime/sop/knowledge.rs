//! StaffDeck SOP production path for `knowledge_query` authority.
//!
//! TS `SopAgentLoop.ts:748-749`: `withKnowledgeAuthority({ ...projection.context,
//! snapshotId, registryGeneration }, () => executeAll([call], ...))`.
use crate::runtime::composition::knowledge_authority::{
    knowledge_authority_from_optional_knowledge, merge_knowledge_authority_into_fields,
};
use pilotdeck_protocol::{JsObject, JsValue};

/// Writes `knowledgeAuthority` on tool fields immediately before `knowledge_query`
/// execution (Rust equivalent of TS `withKnowledgeAuthority` for one tool batch).
pub fn write_knowledge_authority_for_sop_query(
    tool_fields: &mut JsObject,
    optional_knowledge: &JsObject,
) {
    if let Some(authority) = knowledge_authority_from_optional_knowledge(optional_knowledge) {
        tool_fields.insert("knowledgeAuthority".into(), JsValue::Object(authority));
    }
}

/// Stages `optionalKnowledge` on the tool batch `payload.context` object before
/// host tool preparation (SOP loop passes step projection on the execute context).
pub fn stage_optional_knowledge_on_batch_context(
    batch_context: &mut JsObject,
    optional_knowledge: &JsObject,
) {
    batch_context.insert(
        "optionalKnowledge".into(),
        JsValue::Object(optional_knowledge.clone()),
    );
}

/// Production entry used by `tool_context` when building `ToolRuntimeContext`.
pub fn apply_knowledge_authority_for_tool_execution(
    tool_fields: &mut JsObject,
    turn_input: &JsObject,
    batch_context: Option<&JsObject>,
) {
    let projection = turn_input
        .get("sopOptionalKnowledge")
        .or_else(|| {
            batch_context.and_then(|context| {
                context
                    .get("optionalKnowledge")
                    .or_else(|| context.get("sopOptionalKnowledge"))
            })
        })
        .and_then(JsValue::as_object);
    if let Some(projection) = projection {
        write_knowledge_authority_for_sop_query(tool_fields, projection);
        return;
    }
    merge_knowledge_authority_into_fields(tool_fields, turn_input, batch_context);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{JsObject, JsString, JsValue};

    fn s(value: &str) -> JsValue {
        JsValue::String(JsString::from(value))
    }

    fn sample_projection() -> JsObject {
        let mut context = JsObject::new();
        context.insert("sessionKey".into(), s("sess-prod"));
        context.insert("projectKey".into(), s("proj"));
        context.insert("expectedRevision".into(), JsValue::Number(3.0));
        context.insert("requestId".into(), s("req-sop"));
        let mut projection = JsObject::new();
        projection.insert("context".into(), JsValue::Object(context));
        projection.insert("snapshotId".into(), s("snap-prod"));
        projection.insert("registryGeneration".into(), JsValue::Number(11.0));
        projection
    }

    #[test]
    fn write_knowledge_authority_for_sop_query_matches_sop_agent_loop_shape() {
        let mut tool_fields = JsObject::new();
        write_knowledge_authority_for_sop_query(&mut tool_fields, &sample_projection());
        let authority = tool_fields
            .get("knowledgeAuthority")
            .and_then(JsValue::as_object)
            .expect("authority");
        assert_eq!(authority.get("requestId"), Some(&s("req-sop")));
    }
}
