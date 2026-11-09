//! TS `createSopCapabilityAuthorityPort` / `sopCapabilityAuthority.ts:66-80`.
use super::authority_validate::validate_sop_authority_projection;
use crate::runtime::composition::HttpModuleClient;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

pub trait SopAuthorityResolve: Send + Sync {
    fn resolve(
        &self,
        context: JsObject,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<JsObject, JsString>> + Send + '_>>;
}

pub struct HttpSopCapabilityAuthorityPort {
    client: Arc<HttpModuleClient>,
}

impl HttpSopCapabilityAuthorityPort {
    pub fn from_knowledge_binding(knowledge: &JsValue) -> Option<Self> {
        let binding = knowledge.as_object()?.clone();
        let agent_id = binding.get("agentId")?.as_string()?.to_utf8().ok()?;
        let mut binding_value = JsValue::Object(binding);
        for (key, value) in [
            ("implementationId", JsValue::String(JsString::from("staffdeck.sop-capability-authority"))),
            (
                "contract",
                JsValue::String(JsString::from("staffdeck.sop-capability-authority/v1")),
            ),
            (
                "manifestPath",
                JsValue::String(JsString::from(
                    "/api/v1/sop-capability-authority/module-manifest",
                )),
            ),
            (
                "callPath",
                JsValue::String(JsString::from(format!(
                    "/api/v1/agents/{agent_id}/sop-capability-authority/v2/module/call"
                ))),
            ),
            (
                "methods",
                JsValue::Array(vec![JsValue::String(JsString::from("resolve"))]),
            ),
        ] {
            if let JsValue::Object(ref mut obj) = binding_value {
                obj.insert(key.into(), value);
            }
        }
        Some(Self {
            client: Arc::new(HttpModuleClient::new(binding_value)),
        })
    }
}

impl SopAuthorityResolve for HttpSopCapabilityAuthorityPort {
    fn resolve(
        &self,
        context: JsObject,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<JsObject, JsString>> + Send + '_>>
    {
        let client = self.client.clone();
        Box::pin(async move {
            let session_key = context
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
                .ok_or_else(|| JsString::from("SOP_AUTHORITY_RESPONSE_INVALID"))?;
            let request_id = context
                .get("requestId")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| JsString::from("sop-authority.resolve"));
            let mut input = JsObject::new();
            input.insert("operation".into(), JsValue::String(JsString::from("resolve")));
            input.insert("input".into(), JsValue::Object(context.clone()));
            let result = client
                .call(
                    &format!("sop:{session_key}"),
                    "sop-authority.resolve",
                    &request_id.to_utf8().unwrap_or_default(),
                    "capability",
                    "resolve",
                    input,
                    None,
                )
                .await
                .map_err(|e| JsString::from(e.code.as_str()))?;
            validate_sop_authority_projection(&result, &context)
        })
    }
}

pub fn sop_capability_authority_binding(
    snapshot: &JsValue,
    cwd: &JsString,
) -> Option<JsValue> {
    use crate::runtime::composition::is_external_module_binding;
    let sop = snapshot
        .get("config")
        .and_then(|c| c.get("modules"))
        .and_then(|m| m.get("sop"))?;
    let knowledge = snapshot
        .get("config")
        .and_then(|c| c.get("modules"))
        .and_then(|m| m.get("knowledge"))?;
    if !is_external_module_binding(knowledge) {
        return None;
    }
    let impl_id = knowledge
        .get("implementationId")
        .and_then(JsValue::as_string)?;
    if !impl_id.equals_str("staffdeck.knowledge") {
        return None;
    }
    let contract = knowledge.get("contract").and_then(JsValue::as_string)?;
    if !contract.equals_str("staffdeck.knowledge/v1") {
        return None;
    }
    if knowledge.get("credentialEnv").and_then(JsValue::as_string).is_none() {
        return None;
    }
    let agent_id = knowledge.get("agentId").and_then(JsValue::as_string)?;
    let sop_agent = sop.get("discoveryAgentId").and_then(JsValue::as_string)?;
    if !agent_id.equals_str(sop_agent.to_utf8().ok()?.as_str()) {
        return None;
    }
    let discovery = sop.get("discoveryEndpoint").and_then(JsValue::as_string)?;
    let endpoint = knowledge.get("endpoint").and_then(JsValue::as_string)?;
    let discovery_origin = url_origin(discovery)?;
    let knowledge_origin = url_origin(endpoint)?;
    if discovery_origin != knowledge_origin {
        return None;
    }
    let _ = cwd;
    Some(knowledge.clone())
}

fn url_origin(value: &JsString) -> Option<String> {
    let text = value.to_utf8().ok()?;
    let url = text.parse::<url::Url>().ok()?;
    Some(format!(
        "{}://{}",
        url.scheme(),
        url.host_str().unwrap_or_default()
    ))
}
