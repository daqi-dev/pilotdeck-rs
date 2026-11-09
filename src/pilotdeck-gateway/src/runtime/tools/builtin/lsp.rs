//! Model-facing LSP consumer (baseline `tool/builtin/lsp.ts`). Providers own
//! process protocol and lifecycle; this tool owns the public schema, one-based
//! UTF-16 coordinate conversion, workspace contract and result projection.

use super::{object, schema, string, text, tool_error};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{future::Future, pin::Pin, sync::Arc};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspOperation {
    GoToDefinition,
    FindReferences,
    GoToImplementation,
    Hover,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LspQueryRequest {
    pub operation: LspOperation,
    pub file_path: JsString,
    /// Zero-based UTF-16 coordinates passed to the provider.
    pub line: u64,
    pub character: u64,
    pub workspace_root: JsString,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LspQueryResult {
    Locations {
        locations: Vec<JsValue>,
        resolved_workspace_uri: JsString,
    },
    Hover {
        hover: Option<JsValue>,
    },
}

pub type LspFuture = Pin<Box<dyn Future<Output = Result<LspQueryResult, ToolError>> + Send>>;

pub trait LspServicePort: Send + Sync {
    fn query(&self, request: LspQueryRequest, context: ToolRunContext) -> LspFuture;
}

pub struct LspTool {
    fields: JsObject,
    service: Arc<dyn LspServicePort>,
}

impl LspTool {
    pub fn new(service: Arc<dyn LspServicePort>) -> Self {
        Self {
            fields: definition(),
            service,
        }
    }
}

impl ToolDefinition for LspTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}

impl ExecutableTool for LspTool {
    fn validate_input(
        &self,
        input: JsValue,
        _context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move { Ok(validate(&input)) })
    }

    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let service = self.service.clone();
        Box::pin(async move {
            let request = request(&input, &context)?;
            if context.is_aborted() {
                return Err(tool_error("tool_aborted", "LSP query was aborted."));
            }
            let result = service.query(request, context).await?;
            project(result)
        })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("lsp")),
        (
            "description",
            string(
                "Query a language server for precise definitions, references, implementations, or hover information. Coordinates are one-based UTF-16 positions.",
            ),
        ),
        ("kind", string("filesystem")),
        (
            "inputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["operation","file_path","line","character"],"additionalProperties":false,"properties":{"operation":{"type":"string","enum":["goToDefinition","findReferences","goToImplementation","hover"]},"file_path":{"type":"string"},"line":{"type":"integer"},"character":{"type":"integer"}}}"#,
            )),
        ),
    ])
}

fn validate(input: &JsValue) -> Option<Vec<JsObject>> {
    let mut issues = Vec::new();
    if input
        .get("file_path")
        .and_then(JsValue::as_string)
        .is_none_or(|value| text(value).trim().is_empty())
    {
        issues.push(issue("file_path", "file_path must not be empty"));
    }
    for key in ["line", "character"] {
        if input
            .get(key)
            .and_then(JsValue::as_number)
            .is_none_or(|value| value < 1.0 || value.fract() != 0.0 || !value.is_finite())
        {
            issues.push(issue(
                key,
                &format!("{key} must be a positive one-based integer"),
            ));
        }
    }
    (!issues.is_empty()).then_some(issues)
}

fn issue(path: &str, message: &str) -> JsObject {
    object([
        ("path", string(path)),
        ("code", string("invalid_schema")),
        ("message", string(message)),
    ])
}

fn request(input: &JsValue, context: &ToolRunContext) -> Result<LspQueryRequest, ToolError> {
    let workspace_root = super::cwd(context);
    if text(&workspace_root).trim().is_empty() {
        return Err(tool_error(
            "LSP_WORKSPACE_REQUIRED",
            "LSP tool requires a workspace cwd",
        ));
    }
    let operation = match input
        .get("operation")
        .and_then(JsValue::as_string)
        .map(text)
        .as_deref()
    {
        Some("goToDefinition") => LspOperation::GoToDefinition,
        Some("findReferences") => LspOperation::FindReferences,
        Some("goToImplementation") => LspOperation::GoToImplementation,
        Some("hover") => LspOperation::Hover,
        _ => return Err(tool_error("invalid_tool_input", "Unknown LSP operation.")),
    };
    let file_path = input
        .get("file_path")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| tool_error("invalid_tool_input", "file_path must be a string."))?;
    let absolute = if super::is_absolute(&file_path) {
        super::normalize(&file_path)
    } else {
        super::join(&workspace_root, &file_path)
    };
    if !super::within_root(&absolute, &super::normalize(&workspace_root)) {
        return Err(tool_error(
            "LSP_MALFORMED_RESPONSE",
            format!(
                "LSP source {} resolves outside the workspace",
                text(&file_path)
            ),
        ));
    }
    let coordinate = |key: &str| -> Result<u64, ToolError> {
        let value = input.get(key).and_then(JsValue::as_number).ok_or_else(|| {
            tool_error("invalid_tool_input", format!("{key} must be an integer."))
        })?;
        if value < 1.0 || !value.is_finite() || value.fract() != 0.0 || value > u64::MAX as f64 {
            return Err(tool_error(
                "invalid_tool_input",
                format!("{key} must be a positive one-based integer."),
            ));
        }
        Ok(value as u64 - 1)
    };
    Ok(LspQueryRequest {
        operation,
        file_path,
        line: coordinate("line")?,
        character: coordinate("character")?,
        workspace_root,
    })
}

fn project(result: LspQueryResult) -> Result<ToolOutput, ToolError> {
    let (text, data) = match result {
        LspQueryResult::Hover { hover } => {
            let rendered = hover
                .as_ref()
                .map(pretty_json)
                .transpose()?
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "No hover information.".into());
            let mut data = object([("kind", string("hover"))]);
            data.insert("hover".into(), hover.unwrap_or(JsValue::Null));
            (rendered, data)
        }
        LspQueryResult::Locations {
            locations,
            resolved_workspace_uri,
        } => {
            if locations.is_empty() {
                let data = object([
                    ("kind", string("locations")),
                    ("locations", JsValue::Array(Vec::new())),
                    ("resolvedWorkspaceUri", string(resolved_workspace_uri)),
                ]);
                ("No matching locations.".into(), data)
            } else {
                let rendered_value = JsValue::Object(object([
                    ("locations", JsValue::Array(locations.clone())),
                    (
                        "resolvedWorkspaceUri",
                        string(resolved_workspace_uri.clone()),
                    ),
                ]));
                let data = object([
                    ("kind", string("locations")),
                    ("locations", JsValue::Array(locations)),
                    ("resolvedWorkspaceUri", string(resolved_workspace_uri)),
                ]);
                let rendered = pretty_json(&rendered_value)?;
                (rendered, data)
            }
        }
    };
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(text)),
        ]))],
        data: Some(JsValue::Object(data)),
        supplemental_messages: None,
        metadata: None,
    })
}

fn pretty_json(value: &JsValue) -> Result<String, ToolError> {
    let compact = stringify_json(value)
        .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?
        .unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&compact)
        .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
    serde_json::to_string_pretty(&parsed)
        .map_err(|error| tool_error("tool_execution_failed", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_one_based_coordinates() {
        let input = JsValue::Object(object([
            ("file_path", string("")),
            ("line", JsValue::Number(0.0)),
            ("character", JsValue::Number(1.5)),
        ]));
        assert_eq!(validate(&input).unwrap().len(), 3);
    }

    #[test]
    fn projects_empty_results_with_baseline_text() {
        let output = project(LspQueryResult::Locations {
            locations: Vec::new(),
            resolved_workspace_uri: "file:///workspace".into(),
        })
        .unwrap();
        assert_eq!(
            output.content[0].get("text"),
            Some(&string("No matching locations."))
        );
        let output = project(LspQueryResult::Hover { hover: None }).unwrap();
        assert_eq!(
            output.content[0].get("text"),
            Some(&string("No hover information."))
        );
    }
}
