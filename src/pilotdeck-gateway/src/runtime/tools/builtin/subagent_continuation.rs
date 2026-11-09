//! Native `subagent` and `send_message` continuation tools.

use super::{field, object, schema, string};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        session::subagent_continuation::{
            ContinuationAdmission, ContinuationError, ContinuationFuture, ContinuationParent,
            FollowupContinuationRequest, NativeContinuationParentBinding,
            NativeContinuationParentLease, NativeContinuationRuntime, StartContinuationRequest,
            SubagentContinuationManager,
        },
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

#[derive(Clone)]
pub struct SubagentStartPortRequest {
    pub label: JsString,
    pub definition_id: JsString,
    pub prompt: JsString,
    pub cancellation: Option<crate::runtime::network::Cancellation>,
}

#[derive(Clone)]
pub struct SubagentFollowupPortRequest {
    pub child_session_id: JsString,
    pub message: JsString,
    pub cancellation: Option<crate::runtime::network::Cancellation>,
}

pub trait SubagentContinuationPort: Send + Sync {
    fn start(&self, request: SubagentStartPortRequest)
    -> ContinuationFuture<ContinuationAdmission>;
    fn followup(
        &self,
        request: SubagentFollowupPortRequest,
    ) -> ContinuationFuture<ContinuationAdmission>;
}

pub struct InstallSubagentContinuationRequest {
    pub binding: NativeContinuationParentBinding,
    pub provider: JsString,
    pub agent_provider: JsString,
    pub agent_model: JsString,
    pub definitions: Vec<JsString>,
}

/// Session-local consumer ownership. The caller registers `tools()` in its
/// scoped registry and must await `dispose()` before releasing the parent
/// session resources.
pub struct InstalledSubagentContinuation {
    runtime: NativeContinuationRuntime,
    lease: NativeContinuationParentLease,
    port: Arc<dyn SubagentContinuationPort>,
}

impl InstalledSubagentContinuation {
    pub fn port(&self) -> Arc<dyn SubagentContinuationPort> {
        self.port.clone()
    }

    pub fn tools(&self) -> Vec<Arc<dyn ExecutableTool>> {
        subagent_continuation_tools(self.port())
    }

    pub async fn dispose(&self, reason: JsString) -> Result<(), ContinuationError> {
        self.runtime.release_parent(&self.lease, reason).await
    }
}

pub fn install_for_parent(
    runtime: NativeContinuationRuntime,
    request: InstallSubagentContinuationRequest,
) -> Result<InstalledSubagentContinuation, ContinuationError> {
    let parent = request.binding.parent.clone();
    let lease = runtime.bind_parent(request.binding)?;
    let port: Arc<dyn SubagentContinuationPort> = Arc::new(BoundSubagentContinuationPort::new(
        runtime.manager(),
        parent,
        request.provider,
        request.agent_provider,
        request.agent_model,
        request.definitions,
    ));
    Ok(InstalledSubagentContinuation {
        runtime,
        lease,
        port,
    })
}

/// Session-bound production adapter. Exact parent identity and provider/model
/// selection are captured once; tool input cannot replace them.
pub struct BoundSubagentContinuationPort {
    manager: SubagentContinuationManager,
    parent: ContinuationParent,
    provider: JsString,
    agent_provider: JsString,
    agent_model: JsString,
    definitions: Vec<JsString>,
}

impl BoundSubagentContinuationPort {
    pub fn new(
        manager: SubagentContinuationManager,
        parent: ContinuationParent,
        provider: JsString,
        agent_provider: JsString,
        agent_model: JsString,
        definitions: Vec<JsString>,
    ) -> Self {
        Self {
            manager,
            parent,
            provider,
            agent_provider,
            agent_model,
            definitions,
        }
    }
}

impl SubagentContinuationPort for BoundSubagentContinuationPort {
    fn start(
        &self,
        request: SubagentStartPortRequest,
    ) -> ContinuationFuture<ContinuationAdmission> {
        if !self.definitions.contains(&request.definition_id) {
            let definition = text(&request.definition_id);
            return Box::pin(async move {
                Err(ContinuationError::new(
                    "unknown_subagent_type",
                    format!("Unknown subagent type: {definition}"),
                ))
            });
        }
        let manager = self.manager.clone();
        let parent = self.parent.clone();
        let provider = self.provider.clone();
        let agent_provider = self.agent_provider.clone();
        let agent_model = self.agent_model.clone();
        Box::pin(async move {
            manager
                .start(StartContinuationRequest {
                    provider,
                    label: request.label,
                    child_session_id: None,
                    parent,
                    definition_id: request.definition_id,
                    agent_provider,
                    agent_model,
                    prompt: request.prompt,
                    cancellation: request.cancellation,
                })
                .await
        })
    }

    fn followup(
        &self,
        request: SubagentFollowupPortRequest,
    ) -> ContinuationFuture<ContinuationAdmission> {
        let manager = self.manager.clone();
        let parent = self.parent.clone();
        Box::pin(async move {
            manager
                .followup(FollowupContinuationRequest {
                    child_session_id: request.child_session_id,
                    parent,
                    message: request.message,
                    cancellation: request.cancellation,
                })
                .await
        })
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Start,
    Followup,
}

pub struct SubagentContinuationTool {
    kind: Kind,
    fields: JsObject,
    port: Arc<dyn SubagentContinuationPort>,
}

pub fn subagent_continuation_tools(
    port: Arc<dyn SubagentContinuationPort>,
) -> Vec<Arc<dyn ExecutableTool>> {
    [Kind::Start, Kind::Followup]
        .into_iter()
        .map(|kind| {
            Arc::new(SubagentContinuationTool {
                kind,
                fields: definition(kind),
                port: port.clone(),
            }) as Arc<dyn ExecutableTool>
        })
        .collect()
}

fn definition(kind: Kind) -> JsObject {
    let (name, aliases, description, input_schema) = match kind {
        Kind::Start => (
            "subagent",
            vec![string("continuable_subagent")],
            "Start a continuable background subagent. Available types: general-purpose, explore, plan, verify. The call returns after the initial message is durably admitted.",
            r#"{"type":"object","required":["description","prompt"],"additionalProperties":false,"properties":{"description":{"type":"string","description":"Short label for the delegated task."},"prompt":{"type":"string","description":"Initial directive for the background subagent."},"subagent_type":{"type":"string","description":"Subagent definition. Available: general-purpose, explore, plan, verify."}}}"#,
        ),
        Kind::Followup => (
            "send_message",
            vec![],
            "Send a follow-up message to a continuable subagent. The call returns after durable admission.",
            r#"{"type":"object","required":["subagent_id","message"],"additionalProperties":false,"properties":{"subagent_id":{"type":"string","description":"Stable continuable subagent id."},"message":{"type":"string","description":"Message to enqueue for the subagent."}}}"#,
        ),
    };
    let mut fields = object([
        ("name", string(name)),
        ("description", string(description)),
        ("kind", string("agent")),
        ("inputSchema", JsValue::Object(schema(input_schema))),
        (
            "outputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["subagentId","messageId","turnId"],"additionalProperties":false,"properties":{"subagentId":{"type":"string"},"messageId":{"type":"string"},"turnId":{"type":"string"}}}"#,
            )),
        ),
        ("isOpenWorld", JsValue::Bool(true)),
    ]);
    if !aliases.is_empty() {
        fields.insert("aliases".into(), JsValue::Array(aliases));
    }
    fields
}

impl ToolDefinition for SubagentContinuationTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }

    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }

    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }

    fn required_runtime_capabilities(&self) -> Option<Vec<JsString>> {
        Some(vec!["subagent_fork".into()])
    }
}

impl ExecutableTool for SubagentContinuationTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let kind = self.kind;
        let port = self.port.clone();
        Box::pin(async move {
            let cancellation = context.cancellation().cloned();
            let admission = match kind {
                Kind::Start => {
                    let label = required_string(&input, "description")?;
                    let prompt = required_string(&input, "prompt")?;
                    let definition_id = match field(&input, "subagent_type") {
                        JsValue::Undefined | JsValue::Null => "general-purpose".into(),
                        value => value.as_string().cloned().ok_or_else(|| {
                            ToolError::new("invalid_tool_input", "subagent_type must be a string.")
                        })?,
                    };
                    port.start(SubagentStartPortRequest {
                        label,
                        definition_id,
                        prompt,
                        cancellation,
                    })
                    .await
                }
                Kind::Followup => {
                    port.followup(SubagentFollowupPortRequest {
                        child_session_id: required_string(&input, "subagent_id")?,
                        message: required_string(&input, "message")?,
                        cancellation,
                    })
                    .await
                }
            }
            .map_err(tool_error)?;
            Ok(admission_output(admission))
        })
    }
}

fn required_string(input: &JsValue, key: &'static str) -> Result<JsString, ToolError> {
    field(input, key)
        .as_string()
        .cloned()
        .ok_or_else(|| ToolError::new("invalid_tool_input", format!("{key} must be a string.")))
}

fn tool_error(error: ContinuationError) -> ToolError {
    ToolError::new(error.code, error.message)
}

fn admission_output(admission: ContinuationAdmission) -> ToolOutput {
    let data = object([
        (
            "subagentId",
            JsValue::String(admission.child_session_id.clone()),
        ),
        ("messageId", JsValue::String(admission.item_id.clone())),
        ("turnId", JsValue::String(admission.turn_id.clone())),
    ]);
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            (
                "text",
                string(format!(
                    "accepted message {} for subagent {}",
                    text(&admission.item_id),
                    text(&admission.child_session_id)
                )),
            ),
        ]))],
        supplemental_messages: None,
        data: Some(JsValue::Object(data)),
        metadata: None,
    }
}

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
        runtime::tools::ToolRunContext,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct Port {
        starts: Mutex<Vec<(String, String, String)>>,
        followups: Mutex<Vec<(String, String)>>,
    }

    impl SubagentContinuationPort for Port {
        fn start(
            &self,
            request: SubagentStartPortRequest,
        ) -> ContinuationFuture<ContinuationAdmission> {
            self.starts.lock().unwrap().push((
                text(&request.label),
                text(&request.definition_id),
                text(&request.prompt),
            ));
            Box::pin(async {
                Ok(ContinuationAdmission {
                    child_session_id: "child".into(),
                    item_id: "message".into(),
                    turn_id: "turn".into(),
                })
            })
        }

        fn followup(
            &self,
            request: SubagentFollowupPortRequest,
        ) -> ContinuationFuture<ContinuationAdmission> {
            self.followups
                .lock()
                .unwrap()
                .push((text(&request.child_session_id), text(&request.message)));
            Box::pin(async {
                Ok(ContinuationAdmission {
                    child_session_id: "child".into(),
                    item_id: "message-2".into(),
                    turn_id: "turn-2".into(),
                })
            })
        }
    }

    fn context() -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: JsObject::new(),
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: Some(Arc::new(crate::runtime::network::Cancellation::new())),
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: None,
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        }))
    }

    #[test]
    fn definitions_match_ts_contract() {
        let tools = subagent_continuation_tools(Arc::new(Port::default()));
        assert_eq!(tools[0].fields().get("name"), Some(&"subagent".into()));
        assert_eq!(
            tools[0].fields().get("aliases"),
            Some(&JsValue::Array(vec!["continuable_subagent".into()]))
        );
        assert_eq!(tools[1].fields().get("name"), Some(&"send_message".into()));
        for tool in tools {
            assert_eq!(
                tool.is_read_only(&JsValue::Null).unwrap(),
                JsValue::Bool(false)
            );
            assert_eq!(
                tool.is_concurrency_safe(&JsValue::Null).unwrap(),
                JsValue::Bool(true)
            );
            assert_eq!(
                tool.required_runtime_capabilities(),
                Some(vec!["subagent_fork".into()])
            );
        }
    }

    #[tokio::test]
    async fn tools_map_admissions_and_default_definition() {
        let port = Arc::new(Port::default());
        let tools = subagent_continuation_tools(port.clone());
        let start = tools[0]
            .execute(
                JsValue::Object(object([
                    ("description", "proof".into()),
                    ("prompt", "inspect".into()),
                ])),
                context(),
            )
            .await
            .unwrap();
        assert_eq!(start.data.unwrap().get("subagentId"), Some(&"child".into()));
        assert_eq!(
            port.starts.lock().unwrap()[0],
            ("proof".into(), "general-purpose".into(), "inspect".into())
        );

        let followup = tools[1]
            .execute(
                JsValue::Object(object([
                    ("subagent_id", "child".into()),
                    ("message", "continue".into()),
                ])),
                context(),
            )
            .await
            .unwrap();
        assert_eq!(
            followup.data.unwrap().get("messageId"),
            Some(&"message-2".into())
        );
        assert_eq!(
            port.followups.lock().unwrap()[0],
            ("child".into(), "continue".into())
        );
    }
}
