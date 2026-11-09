//! TS `SopAgentLoop.prepareStep` (`SopAgentLoop.ts:495-561`): production writer for
//! `step.optionalKnowledge` after `authorityPort.resolve` (not turn test fields).
use super::{
    authority_port::SopAuthorityResolve,
    authority_validate::{validate_pin_matches_step, validate_sop_authority_projection},
    control_tool_port::SopStepKnowledgeRegistry,
    store,
    value::{field, text},
};
use crate::runtime::composition::is_external_module_binding;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

#[derive(Default)]
pub struct SopPreparedStepStore {
    inner: std::sync::Mutex<std::collections::HashMap<String, JsObject>>,
}

impl SopPreparedStepStore {
    pub fn set_current_step(&self, session: &JsString, step: JsObject) {
        if let Ok(key) = session.to_utf8() {
            self.inner.lock().unwrap().insert(key, step);
        }
    }

    pub fn current_step(&self, session: &JsString) -> Option<JsObject> {
        let key = session.to_utf8().ok()?;
        self.inner.lock().unwrap().get(&key).cloned()
    }

    pub fn optional_knowledge_from_current_step(&self, session: &JsString) -> Option<JsObject> {
        self.current_step(session)
            .and_then(|step| step.get("optionalKnowledge").and_then(JsValue::as_object).cloned())
    }
}

pub fn shared_prepared_step_store() -> Arc<SopPreparedStepStore> {
    static STORE: OnceLock<Arc<SopPreparedStepStore>> = OnceLock::new();
    STORE
        .get_or_init(|| Arc::new(SopPreparedStepStore::default()))
        .clone()
}

pub struct ProductionPrepareInput<'a> {
    pub sessions_root: &'a Path,
    pub session_id: &'a JsString,
    pub turn_id: &'a JsString,
    pub project_key: &'a str,
    pub registry: &'a SopStepKnowledgeRegistry,
    pub prepared: &'a SopPreparedStepStore,
    pub turn: &'a mut JsObject,
    pub authority_binding: Option<&'a JsValue>,
    pub authority: Option<Arc<dyn SopAuthorityResolve>>,
    pub knowledge_query_registered: bool,
}

/// Production write point (grep: `production_prepare_current_step_optional_knowledge`).
pub async fn production_prepare_current_step_optional_knowledge(
    input: ProductionPrepareInput<'_>,
) -> Result<(), JsString> {
    let session_key = input
        .session_id
        .to_utf8()
        .map_err(|_| JsString::from("SOP_AUTHORITY_UNAVAILABLE"))?;
    let persisted = store::read(input.sessions_root, session_key.as_str())
        .map_err(|e| JsString::from(e.code.as_str()))?
        .ok_or_else(|| JsString::from("SOP_AUTHORITY_STATE_MISSING"))?;
    let state = field(&persisted, "state");
    if text(state, "status").as_deref() != Some("active") {
        return Ok(());
    }
    let (mut step, skill_id, sop_version, node_id) = step_from_persisted(&persisted)?;

    let node_type = step
        .get("node")
        .and_then(|v| v.get("type"))
        .and_then(JsValue::as_string)
        .filter(|s| s.equals_str("response"))
        .is_some();

    if node_type
        && input.knowledge_query_registered
        && input.authority_binding.is_some()
        && input.authority.as_ref().is_some()
    {
        let revision = field(&persisted, "revision").clone();
        let mut context = JsObject::new();
        context.insert("sessionKey".into(), JsValue::String(input.session_id.clone()));
        context.insert(
            "projectKey".into(),
            JsValue::String(JsString::from(input.project_key)),
        );
        context.insert("expectedRevision".into(), revision);
        context.insert(
            "requestId".into(),
            JsValue::String(JsString::from(format!(
                "sop.authority:{}:{}",
                session_key,
                input.turn_id.to_utf8().unwrap_or_default()
            ))),
        );
        let authority = input.authority.as_ref().unwrap();
        match authority.resolve(context.clone()).await {
            Ok(projection) => {
                validate_sop_authority_projection(&JsValue::Object(projection.clone()), &context)?;
                validate_pin_matches_step(&projection, &skill_id, &sop_version, &node_id)?;
                super::check_sop_authority_revision(
                    input.sessions_root,
                    &session_key,
                    field(&JsValue::Object(context.clone()), "expectedRevision"),
                )
                .await
                .map_err(|e| JsString::from(e.code.as_str()))?;
                if projection
                    .get("optionalCapabilities")
                    .and_then(|v| match v {
                        JsValue::Array(items) => Some(!items.is_empty()),
                        _ => None,
                    })
                    .unwrap_or(false)
                {
                    step.insert("optionalKnowledge".into(), JsValue::Object(projection));
                }
            }
            Err(code) => {
                step.insert("optionalKnowledgeUnavailable".into(), JsValue::String(code));
            }
        }
    }

    input.prepared.set_current_step(input.session_id, step.clone());
    if let Some(projection) = step.get("optionalKnowledge").and_then(JsValue::as_object) {
        input
            .registry
            .set_step_optional_knowledge(input.session_id, projection);
        input.turn.insert(
            "sopOptionalKnowledge".into(),
            JsValue::Object(projection.clone()),
        );
    }
    Ok(())
}

pub fn production_prepare_current_step_optional_knowledge_blocking(
    input: ProductionPrepareInput<'_>,
) -> Result<(), JsString> {
    block_async(production_prepare_current_step_optional_knowledge(input))
}

fn block_async<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(future)
    }
}

fn step_from_persisted(persisted: &JsValue) -> Result<(JsObject, JsValue, JsValue, JsValue), JsString> {
    let state = field(persisted, "state");
    let skill_id = state
        .get("active_skill_id")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .unwrap_or_else(|| field(state, "selected_skill_id"))
        .clone();
    let node_id = field(state, "active_step_id").clone();
    let JsValue::Array(sops) = field(field(persisted, "bundle"), "sops") else {
        return Err(JsString::from("SOP_AUTHORITY_PIN_INVALID"));
    };
    let sop = sops
        .iter()
        .find(|s| {
            s.get("skill_id")
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .unwrap_or_else(|| field(s, "id"))
                == &skill_id
        })
        .ok_or_else(|| JsString::from("SOP_AUTHORITY_PIN_INVALID"))?;
    let sop_version = field(sop, "version").clone();
    let content = field(sop, "content");
    let node = content
        .get("nodes")
        .and_then(|nodes| match nodes {
            JsValue::Array(items) => items.iter().find(|n| field(n, "node_id") == &node_id).cloned(),
            _ => None,
        })
        .unwrap_or(JsValue::Object(JsObject::new()));
    let mut step = JsObject::new();
    step.insert("skillId".into(), skill_id.clone());
    step.insert("version".into(), sop_version.clone());
    step.insert("nodeId".into(), node_id.clone());
    step.insert("node".into(), node);
    Ok((step, skill_id, sop_version, node_id))
}

pub fn production_refresh_optional_knowledge_from_current_step(
    prepared: &SopPreparedStepStore,
    registry: &SopStepKnowledgeRegistry,
    session: &JsString,
) {
    if let Some(projection) = prepared.optional_knowledge_from_current_step(session) {
        registry.set_step_optional_knowledge(session, &projection);
    }
}

pub fn knowledge_query_available(snapshot: &JsValue) -> bool {
    snapshot
        .get("config")
        .and_then(|c| c.get("modules"))
        .and_then(|m| m.get("knowledge"))
        .is_some_and(is_external_module_binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::sop::store;
    use pilotdeck_protocol::{parse_json, stringify_json};
    use std::fs;

    struct MockAuthority {
        projection: JsObject,
    }

    impl SopAuthorityResolve for MockAuthority {
        fn resolve(
            &self,
            context: JsObject,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<JsObject, JsString>> + Send + '_>,
        > {
            let mut projection = self.projection.clone();
            let mut ctx = projection
                .get("context")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            for key in ["sessionKey", "projectKey", "expectedRevision", "requestId"] {
                if let Some(value) = context.get(key) {
                    ctx.insert(key.into(), value.clone());
                }
            }
            projection.insert("context".into(), JsValue::Object(ctx));
            Box::pin(async move { Ok(projection) })
        }
    }

    fn sample_projection(session: &str) -> JsObject {
        let mut context = JsObject::new();
        context.insert("sessionKey".into(), JsValue::String(JsString::from(session)));
        context.insert("projectKey".into(), JsValue::String(JsString::from("proj")));
        context.insert("expectedRevision".into(), JsValue::Number(2.0));
        context.insert("requestId".into(), JsValue::String(JsString::from("req")));
        let mut cap = JsObject::new();
        cap.insert(
            "operation".into(),
            JsValue::String(JsString::from("knowledge.search/v1")),
        );
        cap.insert(
            "resourceType".into(),
            JsValue::String(JsString::from("knowledge_base")),
        );
        cap.insert("resourceId".into(), JsValue::String(JsString::from("kb")));
        cap.insert("required".into(), JsValue::Bool(false));
        cap.insert(
            "providerModuleId".into(),
            JsValue::String(JsString::from("knowledge.local")),
        );
        cap.insert(
            "providerVersion".into(),
            JsValue::String(JsString::from("1")),
        );
        cap.insert(
            "selectionMode".into(),
            JsValue::String(JsString::from("current")),
        );
        let mut projection = JsObject::new();
        projection.insert("context".into(), JsValue::Object(context));
        projection.insert("sopId".into(), JsValue::String(JsString::from("sop")));
        projection.insert("sopVersion".into(), JsValue::String(JsString::from("1.0.0")));
        projection.insert("nodeId".into(), JsValue::String(JsString::from("lookup")));
        projection.insert("snapshotId".into(), JsValue::String(JsString::from("snap")));
        projection.insert("registryGeneration".into(), JsValue::Number(1.0));
        projection.insert("optionalCapabilities".into(), JsValue::Array(vec![JsValue::Object(cap)]));
        projection
    }

    fn write_active_state(root: &Path, session: &str) {
        fs::create_dir_all(root).unwrap();
        let state = parse_json(
            r#"{"schemaVersion":3,"sessionId":"sess","revision":2,"bundle":{"sops":[{"id":"sop","skill_id":"sop","version":"1.0.0","content":{"nodes":[{"node_id":"lookup","type":"response"}]}}]},"state":{"active_skill_id":"sop","active_step_id":"lookup","status":"active"}}"#,
        )
        .unwrap();
        let mut doc = state.as_object().cloned().unwrap();
        doc.insert("sessionId".into(), JsValue::String(JsString::from(session)));
        fs::write(
            store::path(root, session),
            stringify_json(&JsValue::Object(doc)).unwrap().unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn production_prepare_writes_optional_knowledge_from_authority_resolve() {
        let dir = std::env::temp_dir().join(format!("pdrs-sop-prepare-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let session = "sess-prod";
        write_active_state(&dir, session);
        let registry = SopStepKnowledgeRegistry::default();
        let prepared = SopPreparedStepStore::default();
        let mut turn = JsObject::new();
        let session_id = JsString::from(session);
        let turn_id = JsString::from("turn-1");
        let projection = sample_projection(session);
        production_prepare_current_step_optional_knowledge(ProductionPrepareInput {
            sessions_root: &dir,
            session_id: &session_id,
            turn_id: &turn_id,
            project_key: "proj",
            registry: &registry,
            prepared: &prepared,
            turn: &mut turn,
            authority_binding: Some(&JsValue::Bool(true)),
            authority: Some(Arc::new(MockAuthority { projection })),
            knowledge_query_registered: true,
        })
        .await
        .unwrap();
        assert!(turn.get("sopOptionalKnowledge").is_some());
        assert!(registry.optional_knowledge_for_session(&session_id).is_some());
        assert!(prepared.optional_knowledge_from_current_step(&session_id).is_some());
        let _ = fs::remove_dir_all(dir);
    }
}
