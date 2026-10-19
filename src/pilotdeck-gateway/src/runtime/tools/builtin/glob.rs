//! `glob` (baseline `tool/builtin/glob.ts` + `filesystem/ripgrepFiles.ts`).
//! The TS tool shells out to the bundled ripgrep with
//! `--files --hidden --no-ignore --glob <pattern> --sort=modified`; this port
//! walks the same tree directly (hidden files included, ignore files disabled,
//! `.git`/`node_modules`/`dist` filtered, symlinks not followed) and applies
//! ripgrep's gitignore-style glob semantics. Ordering is modification time
//! ascending, the direction observed from ripgrep 15; exact-mtime ties fall
//! back to the path so the result stays stable.
use super::{
    basename, dirname, is_absolute, object, resolve_workspace_path, schema, string, text,
    tool_error,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::{path::Path, time::UNIX_EPOCH};

const DEFAULT_LIMIT: usize = 1_000;
const IGNORED_DIRECTORIES: &[&str] = &[".git", "node_modules", "dist"];

pub struct GlobTool {
    fields: JsObject,
}

impl GlobTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for GlobTool {
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

impl ExecutableTool for GlobTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { execute(&input, &context) })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("glob")),
        ("aliases", JsValue::Array(vec![string("Glob")])),
        (
            "description",
            string(
                "Fast file pattern matching tool scoped to the workspace.\n\nUsage:\n- Supports glob patterns like \"**/*.js\" or \"src/**/*.ts\".\n- Use this tool when you need to find files by name patterns.\n- Provide the optional path parameter to restrict the search to a subdirectory inside the workspace.\n- Returns matching file paths in stable sorted order.\n- Use this tool to narrow down candidate files before reading or editing them.",
            ),
        ),
        ("kind", string("filesystem")),
        (
            "inputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["pattern"],"additionalProperties":false,"properties":{"pattern":{"type":"string","description":"The glob pattern to match files against. May be workspace-relative, path-relative, or an absolute glob that resolves inside the workspace."},"path":{"type":"string","description":"The directory to search in. If not specified, the workspace root will be used. Omit this field to use the default directory. Must resolve to a directory inside the workspace if provided."},"limit":{"type":"integer","description":"Maximum number of file paths to return. This is a PilotDeck-specific output cap; defaults to 1000. Results remain stable and sorted before truncation."}}}"#,
            )),
        ),
        ("maxResultBytes", JsValue::Number(200_000.0)),
    ])
}

fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let pattern = input
        .get("pattern")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| tool_error("invalid_tool_input", "pattern must be a string."))?;
    let mut search_path = input
        .get("path")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| JsString::from("."));
    let mut search_pattern = pattern.clone();
    if is_absolute(&pattern) {
        let (base, relative) = extract_glob_base_directory(&pattern);
        if !base.is_empty() {
            search_path = base;
            search_pattern = relative;
        }
    }
    let resolved = resolve_workspace_path(&search_path, context, false, false)?;
    let prefix = if resolved.relative.equals_str(".") {
        String::new()
    } else {
        format!("{}/", text(&resolved.relative))
    };
    let files = walk(&super::os_path(&resolved.absolute), &search_pattern)?;
    let limit = input
        .get("limit")
        .and_then(JsValue::as_number)
        .unwrap_or(DEFAULT_LIMIT as f64);
    let selected = slice_limit(&files, limit);
    let truncated = selected.len() < files.len();
    let workspace_files: Vec<String> = selected
        .iter()
        .map(|file| format!("{prefix}{file}"))
        .collect();
    let limit_text = if input.get("limit").is_some() {
        format!(" limit={}", js_number_string(limit))
    } else {
        String::new()
    };
    let mut output_lines = if workspace_files.is_empty() {
        vec!["[No files matched]".to_owned()]
    } else {
        workspace_files.clone()
    };
    output_lines.push(String::new());
    output_lines.push(format!(
        "[glob pagination] returned={} total={} truncated={}{limit_text}",
        workspace_files.len(),
        files.len(),
        if truncated { "true" } else { "false" }
    ));
    if truncated {
        output_lines.push(
            "More files are available. Narrow the pattern/path or call glob again with a higher limit if you need the full list."
                .to_owned(),
        );
    }
    let data_files = JsValue::Array(
        workspace_files
            .into_iter()
            .map(|file| JsValue::String(JsString::from(file)))
            .collect(),
    );
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(output_lines.join("\n"))),
        ]))],
        supplemental_messages: None,
        data: Some(JsValue::Object(object([
            ("files", data_files),
            ("count", JsValue::Number(files.len() as f64)),
            ("truncated", JsValue::Bool(truncated)),
        ]))),
        metadata: Some(object([("truncated", JsValue::Bool(truncated))])),
    })
}

fn js_number_string(value: f64) -> String {
    crate::js_value::number_string(value)
}

/// JS `files.slice(0, limit)` including negative and non-finite limits.
fn slice_limit(files: &[String], limit: f64) -> Vec<String> {
    if limit.is_nan() {
        return Vec::new();
    }
    let length = files.len() as i64;
    let end = if limit.is_infinite() {
        if limit > 0.0 { length } else { 0 }
    } else {
        let value = limit.trunc();
        // ECMAScript `ToIntegerOrInfinity(-0)` is `+0`; `slice(0, -0)` is
        // empty, not "all files". `-0.5` truncates to `-0` for the same reason.
        if value < 0.0 {
            (length as f64 + value).max(0.0) as i64
        } else {
            value.min(length as f64) as i64
        }
    };
    let start = 0;
    let end = end.clamp(0, length);
    files[start..end as usize].to_vec()
}

fn walk(root: &Path, pattern: &JsString) -> Result<Vec<String>, ToolError> {
    let matcher = GlobMatcher::new(pattern)?;
    let mut files: Vec<(String, i64)> = Vec::new();
    walk_directory(root, "", &matcher, &mut files)?;
    files.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| left.0.as_bytes().cmp(right.0.as_bytes()))
    });
    Ok(files.into_iter().map(|(path, _)| path).collect())
}

fn walk_directory(
    directory: &Path,
    relative: &str,
    matcher: &GlobMatcher,
    files: &mut Vec<(String, i64)>,
) -> Result<(), ToolError> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        tool_error(
            "tool_execution_failed",
            format!("ripgrep glob search failed: {error}"),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            tool_error(
                "tool_execution_failed",
                format!("ripgrep glob search failed: {error}"),
            )
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = if relative.is_empty() {
            name.to_string()
        } else {
            format!("{relative}/{name}")
        };
        if path
            .split('/')
            .any(|segment| IGNORED_DIRECTORIES.contains(&segment))
        {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| {
            tool_error(
                "tool_execution_failed",
                format!("ripgrep glob search failed: {error}"),
            )
        })?;
        if file_type.is_dir() {
            if IGNORED_DIRECTORIES.contains(&name.as_ref()) {
                continue;
            }
            walk_directory(&entry.path(), &path, matcher, files)?;
        } else if file_type.is_file() && matcher.matches(&path) {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            let milliseconds = match modified.duration_since(UNIX_EPOCH) {
                Ok(duration) => duration.as_millis() as i64,
                Err(error) => -(error.duration().as_millis() as i64),
            };
            files.push((path, milliseconds));
        }
    }
    Ok(())
}

struct GlobMatcher {
    positive: Option<Regex>,
    negative: bool,
    basename_only: bool,
}

impl GlobMatcher {
    fn new(pattern: &JsString) -> Result<Self, ToolError> {
        let mut value = pattern.units().to_vec();
        let negative = value.first() == Some(&(b'!' as u16));
        if negative {
            value.remove(0);
        }
        let mut anchored = false;
        if value.first() == Some(&(b'/' as u16)) {
            value.remove(0);
            anchored = true;
        }
        let basename_only = !anchored && !value.contains(&(b'/' as u16));
        let text = String::from_utf16_lossy(&value);
        if text.is_empty() || text.ends_with('/') {
            return Ok(Self {
                positive: None,
                negative,
                basename_only,
            });
        }
        let branches = expand_braces(&text);
        let mut sources = Vec::new();
        for branch in branches {
            sources.push(glob_to_regex(&branch));
        }
        let source = if sources.len() == 1 {
            sources.pop().unwrap()
        } else {
            format!("(?:{})", sources.join("|"))
        };
        let regex = Regex::new(&source).map_err(|error| {
            tool_error(
                "tool_execution_failed",
                format!("ripgrep glob search failed: invalid glob pattern: {error}"),
            )
        })?;
        Ok(Self {
            positive: Some(regex),
            negative,
            basename_only,
        })
    }

    fn matches(&self, path: &str) -> bool {
        let Some(regex) = &self.positive else {
            return false;
        };
        let subject = if self.basename_only {
            path.rsplit('/').next().unwrap_or(path)
        } else {
            path
        };
        let matched = regex.is_match(subject);
        matched != self.negative
    }
}

fn glob_to_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut source = String::from("^");
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '*' => {
                if chars.get(index + 1) == Some(&'*') {
                    if chars.get(index + 2) == Some(&'/') {
                        source.push_str("(?:[^/]+/)*");
                        index += 3;
                    } else {
                        source.push_str(".*");
                        index += 2;
                    }
                } else {
                    source.push_str("[^/]*");
                    index += 1;
                }
            }
            '?' => {
                source.push_str("[^/]");
                index += 1;
            }
            '[' => {
                let mut end = index + 1;
                if chars.get(end) == Some(&'!') || chars.get(end) == Some(&'^') {
                    end += 1;
                }
                if chars.get(end) == Some(&']') {
                    end += 1;
                }
                while end < chars.len() && chars[end] != ']' {
                    end += 1;
                }
                if end >= chars.len() {
                    source.push_str("\\[");
                    index += 1;
                } else {
                    let mut class = String::from("[");
                    let mut cursor = index + 1;
                    if chars.get(cursor) == Some(&'!') || chars.get(cursor) == Some(&'^') {
                        class.push('^');
                        cursor += 1;
                    }
                    while cursor < end {
                        let character = chars[cursor];
                        if character == '\\' {
                            class.push_str("\\\\");
                        } else if character == ']' || character == '[' || character == '^' {
                            class.push('\\');
                            class.push(character);
                        } else {
                            class.push(character);
                        }
                        cursor += 1;
                    }
                    class.push(']');
                    source.push_str(&class);
                    index = end + 1;
                }
            }
            '\\' => {
                if let Some(character) = chars.get(index + 1) {
                    push_regex_literal(&mut source, *character);
                    index += 2;
                } else {
                    push_regex_literal(&mut source, '\\');
                    index += 1;
                }
            }
            character => {
                push_regex_literal(&mut source, character);
                index += 1;
            }
        }
    }
    source.push('$');
    source
}

fn push_regex_literal(source: &mut String, character: char) {
    if "\\.^$+()|{}[]?".contains(character) {
        source.push('\\');
    }
    source.push(character);
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut depth = 0;
    let mut open = None;
    for (index, character) in chars.iter().enumerate() {
        match character {
            '\\' => {}
            '{' => {
                if depth == 0 {
                    open = Some(index);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let start = open.take().unwrap();
                    let inner: String = chars[start + 1..index].iter().collect();
                    let mut pieces = Vec::new();
                    let mut piece = String::new();
                    let mut nested = 0;
                    let mut escaped = false;
                    for character in inner.chars() {
                        if escaped {
                            piece.push(character);
                            escaped = false;
                            continue;
                        }
                        match character {
                            '\\' => {
                                piece.push(character);
                                escaped = true;
                            }
                            '{' => {
                                nested += 1;
                                piece.push(character);
                            }
                            '}' => {
                                nested -= 1;
                                piece.push(character);
                            }
                            ',' if nested == 0 => {
                                pieces.push(std::mem::take(&mut piece));
                            }
                            _ => piece.push(character),
                        }
                    }
                    pieces.push(piece);
                    if pieces.len() <= 1 {
                        return vec![pattern.to_owned()];
                    }
                    let prefix: String = chars[..start].iter().collect();
                    let suffix: String = chars[index + 1..].iter().collect();
                    let mut result = Vec::new();
                    for piece in pieces {
                        result.extend(expand_braces(&format!("{prefix}{piece}{suffix}")));
                    }
                    return result;
                }
            }
            _ => {}
        }
    }
    vec![pattern.to_owned()]
}

fn extract_glob_base_directory(pattern: &JsString) -> (JsString, JsString) {
    let units = pattern.units();
    let Some(index) = units
        .iter()
        .position(|unit| matches!(*unit, 0x2a | 0x3f | 0x5b | 0x7b))
    else {
        return (dirname(pattern), basename(pattern));
    };
    let static_prefix = &units[..index];
    let Some(last_separator) = static_prefix.iter().rposition(|unit| *unit == b'/' as u16) else {
        return (JsString::from(""), pattern.clone());
    };
    let base = JsString::from_utf16(units[..last_separator].to_vec());
    let relative = JsString::from_utf16(units[last_separator + 1..].to_vec());
    let base = if base.is_empty() && last_separator == 0 {
        JsString::from("/")
    } else {
        base
    };
    (base, relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        ["a", "b", "c"]
            .iter()
            .map(|value| (*value).to_owned())
            .collect()
    }

    #[test]
    fn slice_limit_matches_js_negative_zero_and_negative_ranges() {
        let files = files();
        assert!(slice_limit(&files, -0.0).is_empty());
        assert!(slice_limit(&files, -0.5).is_empty());
        assert!(slice_limit(&files, 0.0).is_empty());
        assert_eq!(slice_limit(&files, 2.0), vec!["a", "b"]);
        assert_eq!(slice_limit(&files, -1.0), vec!["a", "b"]);
        assert!(slice_limit(&files, -5.0).is_empty());
        assert!(slice_limit(&files, f64::NAN).is_empty());
        assert_eq!(slice_limit(&files, f64::INFINITY), files);
        assert!(slice_limit(&files, f64::NEG_INFINITY).is_empty());
    }
}
