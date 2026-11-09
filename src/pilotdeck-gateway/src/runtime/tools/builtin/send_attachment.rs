//! `send_attachment` (baseline `tool/builtin/sendAttachment.ts`). The native
//! tool emits a file content block that the existing turn projector can route
//! through the active channel. Provider-specific byte snapshotting remains a
//! host composition concern; the default filesystem path is already stable.

use super::{
    StatKind, canonical, cwd, is_absolute, join, normalize, object, resolve_workspace_path, schema,
    stat, string, text, tool_error,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        permission::{PermissionContext, PermissionFuture},
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

pub struct SendAttachmentTool {
    fields: JsObject,
}

impl SendAttachmentTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for SendAttachmentTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for SendAttachmentTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn check_permissions<'a>(
        &'a self,
        input: &'a JsValue,
        context: &'a PermissionContext,
    ) -> PermissionFuture<'a, Option<JsValue>> {
        Box::pin(async move {
            Ok(Some(check_file_permission(
                "send_attachment",
                input,
                context,
                true,
            )))
        })
    }
}

impl ExecutableTool for SendAttachmentTool {
    fn validate_input(
        &self,
        input: JsValue,
        context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move { Ok(validate(&input, &context)) })
    }

    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { execute(&input, &context) })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("send_attachment")),
        ("aliases", JsValue::Array(vec![string("send_file")])),
        ("title", string("Send attachment")),
        (
            "description",
            string(
                "Send a local file back to the user through the current host/channel when supported. Use this when the user asks you to send, upload, return, or share an existing local file. Do not call read_file first just to send binary files; this tool sends the file bytes as an attachment without parsing them. Files under PILOTDECK_WORK_DIR or workspace .pilotdeck/work are internal candidates and cannot be sent directly; publish the reviewed final artifact outside the internal work directory first. Workspace files and registered IM attachment files can be sent directly; paths outside the workspace require explicit user permission before execution.",
            ),
        ),
        ("kind", string("filesystem")),
        (
            "inputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["file_path"],"additionalProperties":false,"properties":{"file_path":{"type":"string","description":"Workspace-relative or absolute path to the file to send."},"name":{"type":"string","description":"Optional filename to display to the recipient."},"mime_type":{"type":"string","description":"Optional MIME type for the file."}}}"#,
            )),
        ),
        (
            "outputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["filePath","name","bytes"],"additionalProperties":false,"properties":{"filePath":{"type":"string"},"name":{"type":"string"},"bytes":{"type":"integer"},"mimeType":{"type":"string"}}}"#,
            )),
        ),
    ])
}

fn validate(input: &JsValue, context: &ToolRunContext) -> Option<Vec<JsObject>> {
    let path = input.get("file_path").and_then(JsValue::as_string)?;
    find_internal_work_root(path, context).map(|root| {
        vec![object([
            ("path", string("file_path")),
            ("code", string("invalid_schema")),
            (
                "message",
                string(internal_attachment_message(&text(path), &text(&root))),
            ),
        ])]
    })
}

fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let file_path = input
        .get("file_path")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| tool_error("invalid_tool_input", "file_path must be a string."))?;
    if let Some(root) = find_internal_work_root(&file_path, context) {
        let mut error = tool_error(
            "invalid_tool_input",
            internal_attachment_message(&text(&file_path), &text(&root)),
        );
        error.details = Some(object([
            ("filePath", string(file_path.clone())),
            ("internalWorkRoot", string(root)),
        ]));
        return Err(error);
    }
    let allow_outside = context
        .fields
        .get("currentPermissionDecision")
        .and_then(|value| value.get("type"))
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str("allow"));
    let resolved = resolve_workspace_path(&file_path, context, true, allow_outside)?;
    let info = stat(&resolved.absolute)?;
    if !matches!(info.kind, StatKind::File) {
        return Err(tool_error(
            "invalid_tool_input",
            format!("Path {} is not a file.", text(&file_path)),
        ));
    }
    let name = input
        .get("name")
        .and_then(JsValue::as_string)
        .map(sanitize_attachment_name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| super::basename(&resolved.absolute));
    let mime = input.get("mime_type").and_then(JsValue::as_string).cloned();
    let mut file_content = object([
        ("type", string("file")),
        ("path", string(resolved.absolute.clone())),
        (
            "description",
            string(format!(
                "Attachment requested for channel delivery: {}",
                text(&name)
            )),
        ),
    ]);
    let mut data = object([
        ("filePath", string(resolved.absolute.clone())),
        ("name", string(name.clone())),
        ("bytes", JsValue::Number(info.size as f64)),
    ]);
    if let Some(mime) = mime {
        file_content.insert("mimeType".into(), string(mime.clone()));
        data.insert("mimeType".into(), string(mime));
    }
    Ok(ToolOutput {
        content: vec![
            JsValue::Object(object([
                ("type", string("text")),
                (
                    "text",
                    string(format!("Sending attachment: {}", text(&name))),
                ),
            ])),
            JsValue::Object(file_content),
        ],
        data: Some(JsValue::Object(data)),
        metadata: Some(object([("attachmentDelivery", JsValue::Bool(true))])),
        supplemental_messages: None,
    })
}

fn sanitize_attachment_name(name: &JsString) -> JsString {
    let cleaned: String = text(name)
        .trim_matches(crate::runtime::tools::js_space)
        .chars()
        .map(|character| {
            if matches!(character, '\\' | '/' | '\0') {
                '_'
            } else {
                character
            }
        })
        .collect();
    JsString::from(cleaned)
}

fn find_internal_work_root(path: &JsString, context: &ToolRunContext) -> Option<JsString> {
    let absolute = if is_absolute(path) {
        normalize(path)
    } else {
        join(&cwd(context), path)
    };
    let mut roots = vec![join(&cwd(context), &JsString::from(".pilotdeck/work"))];
    let configured = context
        .fields
        .get("env")
        .and_then(|value| value.get("PILOTDECK_WORK_DIR"))
        .and_then(JsValue::as_string)
        .map(|value| JsString::from(text(value).trim()))
        .filter(|value| !value.is_empty());
    if let Some(configured) = configured {
        roots.push(if is_absolute(&configured) {
            normalize(&configured)
        } else {
            join(&cwd(context), &configured)
        });
    }
    roots.into_iter().find(|root| {
        super::within_root(&absolute, root)
            || canonical(&absolute)
                .zip(canonical(root))
                .is_some_and(|(path, root)| super::within_root(&path, &root))
    })
}

fn internal_attachment_message(path: &str, root: &str) -> String {
    format!(
        "Cannot send {path} directly because it is inside PilotDeck's internal work directory ({root}). Internal candidates may be incomplete or unreviewed. Publish the reviewed final artifact outside PILOTDECK_WORK_DIR with the relevant skill's delivery workflow (for DOCX, use docx.sh deliver), then send the delivered path."
    )
}

/// Shared TS `checkReadFilePermission` / `checkFilesystemWritePermission` /
/// `checkSendAttachmentPermission` projection. The tool-specific caller
/// selects whether the target must already exist; write_file/edit_file admit a
/// missing leaf while read_file/send_attachment deny it before prompting.
pub(crate) fn check_file_permission(
    tool_name: &str,
    input: &JsValue,
    context: &PermissionContext,
    must_exist: bool,
) -> JsValue {
    let Some(path) = input.get("file_path").and_then(JsValue::as_string) else {
        return deny_permission("Path must be a non-empty string without null bytes.");
    };
    let path_text = text(path);
    if path.is_empty() || path_text.contains('\0') {
        return deny_permission("Path must be a non-empty string without null bytes.");
    }
    let lexical = if is_absolute(path) {
        normalize(path)
    } else {
        join(&context.cwd, path)
    };
    let effective = effective_path(&lexical);
    let roots = std::iter::once(&context.cwd)
        .chain(context.additional_working_directories.iter())
        .map(|root| effective_path(&normalize(root)))
        .collect::<Vec<_>>();
    if roots
        .iter()
        .any(|root| super::within_root(&effective, root))
    {
        // `read_file` and `send_attachment` use `mustExist: true` in the TS
        // resolver. Preserve that distinction before returning the workspace
        // passthrough; a missing in-workspace path is a safety denial, not an
        // implicit permission grant. Write tools intentionally pass
        // `must_exist: false` because their target may be a new file.
        if must_exist && std::fs::metadata(super::os_path(&effective)).is_err() {
            return deny_permission(&format!("File {path_text} does not exist."));
        }
        return JsValue::Object(object([("type", string("passthrough"))]));
    }
    if must_exist && std::fs::metadata(super::os_path(&effective)).is_err() {
        return deny_permission(&format!("File {path_text} does not exist."));
    }

    let directory = super::dirname(&effective);
    let pattern = join(&directory, &JsString::from("*"));
    let reason = JsValue::Object(object([
        ("type", string("tool")),
        ("toolName", string(tool_name)),
        (
            "message",
            string(format!("{tool_name} targets a path outside the workspace.")),
        ),
    ]));
    let rule = JsValue::Object(object([
        ("source", string("session")),
        ("behavior", string("allow")),
        ("toolName", string(tool_name)),
        ("pattern", string(pattern.clone())),
    ]));
    let input_summary = pilotdeck_protocol::stringify_json(&JsValue::Object(object([(
        "file_path",
        string(effective.clone()),
    )])))
    .ok()
    .flatten()
    .unwrap_or_default();
    JsValue::Object(object([
        ("type", string("ask")),
        ("reason", reason.clone()),
        (
            "request",
            JsValue::Object(object([
                ("toolCallId", string("")),
                ("toolName", string(tool_name)),
                ("inputSummary", string(input_summary)),
                ("reason", reason),
                (
                    "options",
                    JsValue::Array(vec![
                        JsValue::Object(object([
                            ("id", string("allow_once")),
                            ("label", string("Allow once")),
                        ])),
                        JsValue::Object(object([
                            ("id", string("allow_session")),
                            ("label", string("Allow this folder for this session")),
                            ("rules", JsValue::Array(vec![rule])),
                        ])),
                        JsValue::Object(object([
                            ("id", string("deny")),
                            ("label", string("Deny")),
                        ])),
                        JsValue::Object(object([
                            ("id", string("cancel")),
                            ("label", string("Cancel")),
                        ])),
                    ]),
                ),
                (
                    "metadata",
                    JsValue::Object(object([
                        ("externalPath", string(effective)),
                        ("allowedDirectory", string(directory)),
                        ("pattern", string(pattern)),
                    ])),
                ),
            ])),
        ),
    ]))
}

fn deny_permission(message: &str) -> JsValue {
    JsValue::Object(object([
        ("type", string("deny")),
        (
            "reason",
            JsValue::Object(object([
                ("type", string("safety")),
                ("message", string(message)),
            ])),
        ),
        ("message", string(message)),
    ]))
}

fn effective_path(path: &JsString) -> JsString {
    if let Some(real) = canonical(path) {
        return real;
    }
    let mut ancestor = super::os_path(path);
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name().map(|name| name.to_owned()) else {
            return normalize(path);
        };
        suffix.push(name);
        if !ancestor.pop() {
            return normalize(path);
        }
    }
    let Ok(mut real) = std::fs::canonicalize(ancestor) else {
        return normalize(path);
    };
    for component in suffix.into_iter().rev() {
        real.push(component);
    }
    super::js_path(&real)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_only_path_separators_and_nulls() {
        assert_eq!(
            text(&sanitize_attachment_name(&" ../a\\b\0.txt ".into())),
            ".._a_b_.txt"
        );
        assert!(sanitize_attachment_name(&"   ".into()).is_empty());
    }

    #[test]
    fn definition_has_ts_contract() {
        let fields = definition();
        assert!(fields.get("inputSchema").is_some());
        assert!(fields.get("outputSchema").is_some());
        assert_eq!(
            fields.get("aliases"),
            Some(&JsValue::Array(vec![string("send_file")]))
        );
    }
}
