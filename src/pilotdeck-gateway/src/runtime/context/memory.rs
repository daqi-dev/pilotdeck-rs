//! Failure-tolerant memory attachments (`MemoryAttachmentBuilder.ts:25-89`).
//! The provider is injected; this layer owns only request cancellation, timeout
//! and attachment rendering, never the provider's retrieval/truncation policy.
use crate::{
    host::{Cancellation, HostError, ModuleError},
    metadata::js_trim,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, time::Duration};

pub struct MemoryRetrieveInput {
    pub query: String,
    pub session_id: String,
    pub project_root: String,
    pub recent_messages: Vec<JsValue>,
    pub signal: Cancellation,
}
#[derive(Clone, Debug)]
pub struct MemoryDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
}
#[derive(Default)]
pub struct MemoryRetrieveResult {
    pub system_context: Option<String>,
    pub diagnostics: Vec<MemoryDiagnostic>,
}
pub type MemoryFuture =
    Pin<Box<dyn Future<Output = Result<MemoryRetrieveResult, String>> + Send + 'static>>;

/// `MemoryResolver.ts:23-28` `MemoryCaptureTurnInput`.
pub struct MemoryCaptureTurnInput {
    pub session_id: String,
    pub project_root: String,
    pub messages: Vec<JsValue>,
    /// TS `MemoryCaptureTurnInput.errored` is declared boolean but the runtime
    /// forwards it without coercion; keep the wire value.
    pub errored: JsValue,
}

pub type MemoryCaptureFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;
pub trait MemoryResolver: Send + Sync {
    fn retrieve(&self, input: MemoryRetrieveInput) -> MemoryFuture;
    fn capture_turn(&self, input: MemoryCaptureTurnInput) -> MemoryCaptureFuture;
}

/// `MemoryResolver.ts:3-7` `ContextMemoryMessage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextMemoryMessage {
    pub msg_id: JsString,
    pub role: JsString,
    pub content: JsString,
}
impl ContextMemoryMessage {
    pub fn to_js(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("msgId".into(), JsValue::String(self.msg_id.clone()));
        object.insert("role".into(), JsValue::String(self.role.clone()));
        object.insert("content".into(), JsValue::String(self.content.clone()));
        JsValue::Object(object)
    }
}

/// `MemoryResolver.ts:41-43` `CanonicalMessagesToMemoryMessagesOptions`.
/// `None` is the omitted property, so the default includes fork carryover.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanonicalMessagesToMemoryMessagesOptions {
    pub include_fork_carryover: Option<bool>,
}

/// TS `message.metadata?.forkCarryover` truthiness. Optional chaining only
/// short-circuits null/undefined, and the filter keeps every falsy value.
pub(super) fn fork_carryover(message: &JsValue) -> bool {
    message
        .get("metadata")
        .and_then(|metadata| metadata.get("forkCarryover"))
        .is_some_and(super::compaction::js_truthy)
}

/// `MemoryResolver.ts:45-87` `canonicalMessagesToMemoryMessages`. Strings keep
/// their raw UTF-16 units: lone surrogates are not whitespace and must not be
/// replacement-decoded or dropped.
pub fn canonical_messages_to_memory_messages(
    messages: &[JsValue],
    options: CanonicalMessagesToMemoryMessagesOptions,
) -> Result<Vec<ContextMemoryMessage>, String> {
    let include_fork_carryover = options.include_fork_carryover != Some(false);
    let mut output = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if !include_fork_carryover && fork_carryover(message) {
            continue;
        }
        let role = message
            .get("role")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| "Canonical message role is not a string.".to_owned())?;
        let Some(JsValue::Array(content)) = message.get("content") else {
            return Err("Canonical message content is not an array.".into());
        };
        let mut entries: Vec<(JsString, JsString)> = Vec::new();
        for block in content {
            match block.get("type").and_then(JsValue::as_string) {
                Some(kind) if kind.equals_str("text") => {
                    let text = block
                        .get("text")
                        .and_then(JsValue::as_string)
                        .ok_or_else(|| "Canonical text block is not a string.".to_owned())?;
                    push_entry(&mut entries, &role, text);
                }
                Some(kind) if kind.equals_str("tool_result") => {
                    let text = tool_result_text(block)?;
                    push_entry(&mut entries, &JsString::from("tool"), &text);
                }
                Some(kind)
                    if kind.equals_str("tool_result_reference")
                        || kind.equals_str("media_reference") =>
                {
                    let preview = block
                        .get("preview")
                        .and_then(JsValue::as_string)
                        .ok_or_else(|| "Canonical reference preview is not a string.".to_owned())?;
                    push_entry(&mut entries, &JsString::from("tool"), preview);
                }
                _ => {}
            }
        }
        let entry_count = entries.len();
        for (entry_index, (entry_role, content)) in entries.into_iter().enumerate() {
            let msg_id = if entry_count == 1 {
                format!("message-{index}")
            } else {
                format!("message-{index}:{entry_index}")
            };
            output.push(ContextMemoryMessage {
                msg_id: msg_id.into(),
                role: entry_role,
                content,
            });
        }
    }
    Ok(output)
}

fn push_entry(entries: &mut Vec<(JsString, JsString)>, role: &JsString, text: &JsString) {
    let trimmed = crate::runtime::memory::js_trim_units(text.units());
    if trimmed.is_empty() {
        return;
    }
    if let Some((previous_role, previous_content)) = entries.last_mut()
        && previous_role.units() == role.units()
    {
        let mut units = previous_content.units().to_vec();
        units.push(u16::from(b'\n'));
        units.extend_from_slice(trimmed);
        *previous_content = JsString::from_utf16(units);
        return;
    }
    entries.push((role.clone(), JsString::from_utf16(trimmed.to_vec())));
}

fn tool_result_text(block: &JsValue) -> Result<JsString, String> {
    let Some(JsValue::Array(items)) = block.get("content") else {
        return Err("Canonical tool_result content is not an array.".into());
    };
    let mut units = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        let text = match item.get("type").and_then(JsValue::as_string) {
            Some(kind) if kind.equals_str("text") => item
                .get("text")
                .and_then(JsValue::as_string)
                .map(|value| value.units().to_vec())
                .unwrap_or_default(),
            _ => {
                let label = match item.get("type") {
                    Some(value) => crate::js_value::display(value).unwrap_or_default(),
                    None => "undefined".to_owned(),
                };
                format!("[{label}]").encode_utf16().collect()
            }
        };
        units.extend(text);
    }
    Ok(JsString::from_utf16(units))
}

/// Build one memory attachment with the resolver's timeout and parent signal.
pub async fn build(
    resolver: &dyn MemoryResolver,
    mut input: MemoryRetrieveInput,
    parent: Option<&Cancellation>,
    timeout_ms: f64,
) -> (Option<String>, Vec<MemoryDiagnostic>) {
    if parent.is_some_and(Cancellation::is_cancelled) {
        return (None, Vec::new());
    }
    let controller = Cancellation::default();
    input.signal = controller.clone();
    let retrieve = resolver.retrieve(input);
    let cancelled = async {
        match parent {
            Some(parent) => parent.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let timeout = async {
        if timeout_ms > 0.0 {
            let delay = crate::runtime::network::node_timer_delay_ms(timeout_ms);
            tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        } else {
            std::future::pending().await
        }
    };
    let result = tokio::select! {
        biased;
        result = retrieve => result,
        () = cancelled => {
            controller.cancel(None);
            return (None, Vec::new());
        }
        () = timeout => {
            controller.cancel(None);
            Err(format!("MemoryResolver.retrieve timed out after {}ms.", crate::js_value::display(&JsValue::Number(timeout_ms)).unwrap_or_default()))
        }
    };
    // A provider can abort the parent and resolve in the same poll. TS's
    // forwarding listener has already aborted the child in that case.
    if parent.is_some_and(Cancellation::is_cancelled) {
        controller.cancel(None);
    }
    match result {
        Ok(result) => {
            let context = result
                .system_context
                .as_deref()
                .map(js_trim)
                .filter(|s| !s.is_empty())
                .map(|s| format!("<memory-context>\n{s}\n</memory-context>"));
            (context, result.diagnostics)
        }
        Err(error) => {
            if parent.is_some_and(Cancellation::is_cancelled) {
                return (None, Vec::new());
            }
            let message = if controller.is_cancelled() {
                error
            } else {
                format!("MemoryResolver.retrieve failed: {error}")
            };
            (
                None,
                vec![MemoryDiagnostic {
                    code: "memory_provider_error".into(),
                    severity: "warning".into(),
                    message,
                }],
            )
        }
    }
}

/// `DefaultContextRuntime.ts:633-644` `extractRecentUserText`, including its
/// failure mode: a `type: "text"` block whose `text` is missing or not a
/// string throws at `block.text.trim()`, rejecting the whole prepare, instead
/// of skipping the block.
pub(super) fn recent_user_text(messages: &[JsValue]) -> Result<String, HostError> {
    for message in messages.iter().rev() {
        if !message
            .get("role")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("user"))
        {
            continue;
        }
        if let Some(JsValue::Array(content)) = message.get("content") {
            for block in content {
                if block
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("text"))
                {
                    let text = block.get("text").cloned().unwrap_or(JsValue::Undefined);
                    let text = text.as_string().cloned().ok_or_else(|| {
                        ModuleError::new(match &text {
                            JsValue::Undefined => {
                                "Cannot read properties of undefined (reading 'trim')"
                            }
                            JsValue::Null => "Cannot read properties of null (reading 'trim')",
                            _ => "block.text.trim is not a function",
                        })
                    })?;
                    let text = String::from_utf16_lossy(text.units());
                    if !js_trim(&text).is_empty() {
                        return Ok(text);
                    }
                }
            }
        }
    }
    Ok(String::new())
}
