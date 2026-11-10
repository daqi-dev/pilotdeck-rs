//! Native attachment inspection. The host consumes the file result; this tool
//! does not upload bytes or invent a successful channel delivery receipt.
use super::{
    StatKind, basename, canonical, cwd, dirname, field, is_absolute, join, normalize, object,
    resolve_workspace_path, schema, stat, string, text, tool_error, within_root,
};
use crate::{
    host::{
        HostError,
        modules::{
            capability::ToolDefinition,
            tool_context::{HostToolCheckpoint, ToolRuntimeContext},
        },
    },
    runtime::{
        permission::{PermissionContext, PermissionFuture},
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

pub struct SendAttachmentTool {
    fields: JsObject,
    checkpoint: HostToolCheckpoint,
    env: JsObject,
}
impl SendAttachmentTool {
    pub fn new(checkpoint: HostToolCheckpoint, env: JsObject) -> Self {
        Self {
            checkpoint,
            env,
            fields: object([
                ("name", string("send_attachment")),
                ("aliases", JsValue::Array(vec![string("send_file")])),
                ("title", string("Send attachment")),
                ("kind", string("filesystem")),
                (
                    "description",
                    string(
                        "Send a local file back to the user through the current host/channel when supported. Use this when the user asks you to send, upload, return, or share an existing local file. Do not call read_file first just to send binary files; this tool sends the file bytes as an attachment without parsing them. Files under PILOTDECK_WORK_DIR or workspace .pilotdeck/work are internal candidates and cannot be sent directly; publish the reviewed final artifact outside the internal work directory first. Workspace files and registered IM attachment files can be sent directly; paths outside the workspace require explicit user permission before execution.",
                    ),
                ),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r###"{"type":"object","required":["file_path"],"additionalProperties":false,"properties":{"file_path":{"type":"string","description":"Workspace-relative or absolute path to the file to send."},"name":{"type":"string","description":"Optional filename to display to the recipient."},"mime_type":{"type":"string","description":"Optional MIME type for the file."}}}"###,
                    )),
                ),
                (
                    "outputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","required":["filePath","name","bytes"],"additionalProperties":false,"properties":{"filePath":{"type":"string"},"name":{"type":"string"},"bytes":{"type":"integer"},"mimeType":{"type":"string"}}}"#,
                    )),
                ),
            ]),
        }
    }
    fn permission_context(&self, permission: &PermissionContext) -> ToolRunContext {
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: object([
                ("cwd", string(permission.cwd.clone())),
                (
                    "permissionMode",
                    string(permission.mode.as_js_string().clone()),
                ),
                ("permissionContext", permission.to_value()),
                ("env", JsValue::Object(self.env.clone())),
            ]),
            files: self.checkpoint.tool_context_state(),
            abort_signal: None,
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
}
impl ToolDefinition for SendAttachmentTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
    fn check_permissions<'a>(
        &'a self,
        input: &'a JsValue,
        permission: &'a PermissionContext,
    ) -> PermissionFuture<'a, Option<JsValue>> {
        let input = input.clone();
        let context = self.permission_context(permission);
        Box::pin(async move { Ok(Some(check_permission(&input, &context))) })
    }
}
impl ExecutableTool for SendAttachmentTool {
    fn validate_input(
        &self,
        input: JsValue,
        context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move {
            Ok(internal_root(&input_path(&input)?, &context).map(|root| {
                vec![object([
                    ("path", string("file_path")),
                    ("code", string("invalid_schema")),
                    (
                        "message",
                        string(internal_message(
                            &input_path(&input).expect("validated path"),
                            &root,
                        )),
                    ),
                ])]
            }))
        })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { execute(&input, &context) })
    }
}
fn input_path(input: &JsValue) -> Result<JsString, ToolError> {
    field(input, "file_path")
        .as_string()
        .cloned()
        .ok_or_else(|| tool_error("invalid_tool_input", "file_path must be a string."))
}
fn internal_root(path: &JsString, context: &ToolRunContext) -> Option<JsString> {
    let cwd = cwd(context);
    let absolute = if is_absolute(path) {
        normalize(path)
    } else {
        join(&cwd, path)
    };
    let mut roots = vec![join(&cwd, &".pilotdeck/work".into())];
    if let Some(configured) = context
        .fields
        .get("env")
        .and_then(|v| v.get("PILOTDECK_WORK_DIR"))
        .and_then(JsValue::as_string)
    {
        let configured = text(configured)
            .trim_matches(crate::runtime::tools::js_space)
            .to_owned();
        if !configured.is_empty() {
            let configured = JsString::from(configured);
            roots.push(if is_absolute(&configured) {
                normalize(&configured)
            } else {
                join(&cwd, &configured)
            });
        }
    }
    if let Some(root) = roots.iter().find(|root| within_root(&absolute, root)) {
        return Some(root.clone());
    }
    let real = canonical(&absolute)?;
    roots
        .into_iter()
        .find(|root| canonical(root).is_some_and(|root| within_root(&real, &root)))
}
fn internal_message(path: &JsString, root: &JsString) -> String {
    format!(
        "Cannot send {} directly because it is inside PilotDeck's internal work directory ({}). Internal candidates may be incomplete or unreviewed. Publish the reviewed final artifact outside PILOTDECK_WORK_DIR with the relevant skill's delivery workflow (for DOCX, use docx.sh deliver), then send the delivered path.",
        text(path),
        text(root)
    )
}
fn deny(error: ToolError) -> JsValue {
    JsValue::Object(object([
        ("type", string("deny")),
        (
            "reason",
            JsValue::Object(object([
                ("type", string("safety")),
                ("message", string(error.message.clone())),
            ])),
        ),
        ("message", string(error.message)),
    ]))
}
fn check_permission(input: &JsValue, context: &ToolRunContext) -> JsValue {
    let path = match input_path(input) {
        Ok(path) => path,
        Err(error) => return deny(error),
    };
    match resolve_workspace_path(&path, context, true, false) {
        Ok(_) => return JsValue::Object(object([("type", string("passthrough"))])),
        Err(error) if error.code == "path_not_allowed" => {}
        Err(error) => return deny(error),
    }
    let resolved = match resolve_workspace_path(&path, context, false, true) {
        Ok(path) => path,
        Err(error) => return deny(error),
    };
    let directory = dirname(&resolved.absolute);
    let pattern = join(&directory, &"*".into());
    let rule = object([
        ("source", string("session")),
        ("behavior", string("allow")),
        ("toolName", string("send_attachment")),
        ("pattern", string(pattern.clone())),
    ]);
    let reason = JsValue::Object(object([
        ("type", string("tool")),
        ("toolName", string("send_attachment")),
        (
            "message",
            string("send_attachment targets a path outside the workspace."),
        ),
    ]));
    let summary = pilotdeck_protocol::stringify_json(&JsValue::Object(object([(
        "file_path",
        string(resolved.absolute.clone()),
    )])))
    .expect("path object serializes")
    .expect("path object is defined");
    JsValue::Object(object([
        ("type", string("ask")),
        ("reason", reason.clone()),
        (
            "request",
            JsValue::Object(object([
                ("toolCallId", string("")),
                ("toolName", string("send_attachment")),
                ("inputSummary", string(summary)),
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
                            ("rules", JsValue::Array(vec![JsValue::Object(rule)])),
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
                        ("externalPath", string(resolved.absolute)),
                        ("allowedDirectory", string(directory)),
                        ("pattern", string(pattern)),
                    ])),
                ),
            ])),
        ),
    ]))
}
fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let path = input_path(input)?;
    if let Some(root) = internal_root(&path, context) {
        let mut error = tool_error("invalid_tool_input", internal_message(&path, &root));
        error.details = Some(object([
            ("filePath", string(path)),
            ("internalWorkRoot", string(root)),
        ]));
        return Err(error);
    }
    let authorized = context
        .fields
        .get("currentPermissionDecision")
        .and_then(|v| v.get("type"))
        == Some(&string("allow"));
    let resolved = resolve_workspace_path(&path, context, true, authorized)?;
    let metadata = stat(&resolved.absolute)?;
    if !matches!(metadata.kind, StatKind::File) {
        return Err(tool_error(
            "invalid_tool_input",
            format!("Path {} is not a file.", text(&path)),
        ));
    }
    let name = field(input, "name")
        .as_string()
        .map(|v| {
            // Sanitize UTF-16 units without replacing unpaired surrogates.
            let units = v.units();
            let white =
                |u: u16| char::from_u32(u as u32).is_some_and(crate::runtime::tools::js_space);
            let start = units.iter().position(|u| !white(*u)).unwrap_or(units.len());
            let end = units
                .iter()
                .rposition(|u| !white(*u))
                .map_or(start, |i| i + 1);
            JsString::from_utf16(
                units[start..end]
                    .iter()
                    .map(|u| if matches!(*u, 0 | 47 | 92) { 95 } else { *u })
                    .collect::<Vec<_>>(),
            )
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| basename(&resolved.absolute));
    let mut data = object([
        ("filePath", string(resolved.absolute.clone())),
        ("name", string(name.clone())),
        ("bytes", JsValue::Number(metadata.size as f64)),
    ]);
    let mut file = object([
        ("type", string("file")),
        ("path", string(resolved.absolute)),
        (
            "description",
            string(format!(
                "Attachment requested for channel delivery: {}",
                text(&name)
            )),
        ),
    ]);
    if let Some(mime) = field(input, "mime_type")
        .as_string()
        .filter(|v| !v.is_empty())
    {
        data.insert("mimeType".into(), string(mime.clone()));
        file.insert("mimeType".into(), string(mime.clone()));
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
            JsValue::Object(file),
        ],
        data: Some(JsValue::Object(data)),
        metadata: Some(object([("attachmentDelivery", JsValue::Bool(true))])),
        ..Default::default()
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    struct Root(std::path::PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn wire(value: &JsValue, root: &str) -> Value {
        serde_json::from_str(
            &pilotdeck_protocol::stringify_json(value)
                .unwrap()
                .unwrap()
                .replace(root, "<root>"),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn empty_or_nul_paths_are_denied_before_attachment_execution() {
        let tool = SendAttachmentTool::new(HostToolCheckpoint::default(), JsObject::new());
        let permission = crate::runtime::permission::create_default_permission_context(
            crate::runtime::permission::DefaultPermissionContextOptions {
                cwd: "/isolated-workspace".into(),
                ..Default::default()
            },
        );
        for path in ["", "a\0b"] {
            let input = JsValue::Object(object([("file_path", string(path))]));
            let decision = tool
                .check_permissions(&input, &permission)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(decision.get("type"), Some(&string("deny")));
            assert_eq!(
                decision.get("message"),
                Some(&string(
                    "Path must be a non-empty string without null bytes."
                ))
            );
            let error = tool
                .execute(input, tool.permission_context(&permission))
                .await
                .unwrap_err();
            assert_eq!(
                error.message,
                "Path must be a non-empty string without null bytes.".into()
            );
        }
    }
    #[tokio::test]
    async fn real_files_and_permissions_match_fixed_ts_attachment_oracle() {
        let path = std::env::temp_dir().join(format!("pdrs-attachment-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let root = Root(std::fs::canonicalize(path).unwrap());
        let project = root.0.join("project");
        std::fs::create_dir_all(project.join(".pilotdeck/work")).unwrap();
        std::fs::create_dir(project.join("custom")).unwrap();
        std::fs::create_dir(root.0.join("home")).unwrap();
        std::fs::write(project.join("final.bin"), [0, 255, 9]).unwrap();
        std::fs::write(project.join(".pilotdeck/work/draft"), "draft").unwrap();
        std::fs::write(project.join("custom/draft"), "draft").unwrap();
        std::fs::write(root.0.join("external.bin"), "external").unwrap();
        std::os::unix::fs::symlink(project.join(".pilotdeck/work/draft"), project.join("alias"))
            .unwrap();
        std::os::unix::fs::symlink(root.0.join("external.bin"), project.join("outside-alias"))
            .unwrap();
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/send-attachment/oracle.json"
        ))
        .unwrap();
        for row in oracle["rows"].as_array().unwrap() {
            let checkpoint = HostToolCheckpoint::default();
            if row["registered"] == true {
                checkpoint.allow_read_files(&[root
                    .0
                    .join("external.bin")
                    .to_string_lossy()
                    .into_owned()
                    .into()]);
            }
            let mut env = object([(
                "PILOT_HOME",
                string(root.0.join("home").to_string_lossy().into_owned()),
            )]);
            if let Some(work) = row["work"].as_str() {
                env.insert("PILOTDECK_WORK_DIR".into(), string(work));
            }
            let tool = SendAttachmentTool::new(checkpoint, env);
            let permission = crate::runtime::permission::create_default_permission_context(
                crate::runtime::permission::DefaultPermissionContextOptions {
                    cwd: project.to_string_lossy().into_owned().into(),
                    ..Default::default()
                },
            );
            let mut context = tool.permission_context(&permission);
            if row["allow"] == true {
                context.fields.insert(
                    "currentPermissionDecision".into(),
                    JsValue::Object(object([("type", string("allow"))])),
                );
            }
            let input = pilotdeck_protocol::parse_json(&row["input"].to_string()).unwrap();
            let issues = tool
                .validate_input(input.clone(), context.clone())
                .await
                .unwrap();
            let validation = match issues {
                None => json!({"ok":true,"input":row["input"]}),
                Some(issues) => {
                    json!({"ok":false,"issues":wire(&JsValue::Array(issues.into_iter().map(JsValue::Object).collect()),root.0.to_str().unwrap())})
                }
            };
            assert_eq!(validation, row["validation"], "validation {}", row["id"]);
            assert_eq!(
                wire(
                    &tool
                        .check_permissions(&input, &permission)
                        .await
                        .unwrap()
                        .unwrap(),
                    root.0.to_str().unwrap()
                ),
                row["permission"],
                "permission {}",
                row["id"]
            );
            let actual = match tool.execute(input, context).await {
                Ok(out) => json!({"ok":true,"output":{
                    "content":wire(&JsValue::Array(out.content),root.0.to_str().unwrap()),
                    "data":wire(&out.data.unwrap(),root.0.to_str().unwrap()),
                    "metadata":wire(&JsValue::Object(out.metadata.unwrap()),root.0.to_str().unwrap()),
                }}),
                Err(error) => {
                    let mut e = json!({"code":error.code,"message":text(&error.message).replace(root.0.to_str().unwrap(),"<root>")});
                    if let Some(details) = error.details {
                        e["details"] = wire(&JsValue::Object(details), root.0.to_str().unwrap());
                    }
                    json!({"ok":false,"error":e})
                }
            };
            assert_eq!(actual, row["result"], "execute {}", row["id"]);
        }
    }
}
