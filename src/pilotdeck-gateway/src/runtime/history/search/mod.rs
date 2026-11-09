//! Shared native JSONL search extracted from pilotdeck-cli/chat_search.
//! This module owns no argv, stdout, session writer, retention or CLI dependency.
mod matcher;
#[cfg(test)]
mod tests;
mod title;
use crate::{list_session_entries, read_session_bytes, session_chat_dir};
use matcher::Matcher;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
#[derive(Clone)]
struct Text(JsString);
impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf16_lossy(self.0.units()))
    }
}
#[derive(Debug, Clone)]
pub enum SearchError {
    Error(String),
    Message(JsString),
}
impl SearchError {
    pub fn message(&self) -> JsString {
        match self {
            Self::Error(s) => s.as_str().into(),
            Self::Message(s) => s.clone(),
        }
    }
}
impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf16_lossy(self.message().units()))
    }
}
impl std::error::Error for SearchError {}
#[derive(Clone, Debug)]
pub struct SearchOptions {
    /// Explicit caller cwd keeps CLI relative-project and all-project/session behavior.
    pub cwd: Option<PathBuf>,
    pub pilot_home: PathBuf,
    pub project_root: Option<PathBuf>,
    pub query: JsString,
    pub limit: f64,
    pub regex: bool,
    pub case_sensitive: bool,
    pub role: JsString,
    pub session_id: Option<JsString>,
}
impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            pilot_home: PathBuf::new(),
            project_root: None,
            query: JsString::default(),
            limit: 20.0,
            regex: false,
            case_sensitive: false,
            role: "all".into(),
            session_id: None,
        }
    }
}
struct Match {
    session_id: Text,
    role: &'static str,
    text: Text,
    snippet: Text,
    created_at: Text,
    line_number: usize,
    session_title: Text,
    project_key: String,
}
struct SearchResult {
    query: JsString,
    matches: Vec<Match>,
    truncated: bool,
    sessions_scanned: usize,
}
impl SearchResult {
    fn into_value(self) -> JsValue {
        let mut matches = Vec::with_capacity(self.matches.len());
        for item in self.matches {
            matches.push(object([
                ("sessionId", JsValue::String(item.session_id.0)),
                ("role", item.role.into()),
                ("text", JsValue::String(item.text.0)),
                ("snippet", JsValue::String(item.snippet.0)),
                ("createdAt", JsValue::String(item.created_at.0)),
                ("lineNumber", JsValue::Number(item.line_number as f64)),
                ("sessionTitle", JsValue::String(item.session_title.0)),
                ("projectKey", JsValue::from(item.project_key.as_str())),
            ]));
        }
        object([
            ("query", JsValue::String(self.query)),
            ("matches", JsValue::Array(matches)),
            ("truncated", JsValue::Bool(self.truncated)),
            (
                "sessionsScanned",
                JsValue::Number(self.sessions_scanned as f64),
            ),
        ])
    }
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut out = JsObject::new();
    for (k, v) in fields {
        out.insert(k.into(), v);
    }
    JsValue::Object(out)
}
struct Target {
    path: PathBuf,
    project: String,
}
/// Synchronous filesystem work; async consumers run this on their blocking pool.
/// Empty queries return before any IO, matching SessionSearchPort rather than CLI usage.
pub fn search(options: SearchOptions) -> Result<JsValue, SearchError> {
    let query = trim(&Text(options.query.clone())).0;
    if query.is_empty() {
        return Ok(SearchResult {
            query,
            matches: vec![],
            truncated: false,
            sessions_scanned: 0,
        }
        .into_value());
    }
    let limit = options.limit.clamp(1.0, 100.0);
    let role = options
        .role
        .to_utf8()
        .unwrap_or_else(|_| "invalid-role".into());
    let matcher = Matcher::new_units(&query, options.regex, options.case_sensitive)?;
    let files = collect(&options)?;
    let mut result = SearchResult {
        query,
        matches: vec![],
        truncated: false,
        sessions_scanned: files.len(),
    };
    // The TS provider indexes every file before searching or applying the limit.
    let mut titles = HashMap::new();
    for file in &files {
        let bytes = read_session_bytes(&file.path).unwrap_or_default();
        if let Some(title) = title::read(&bytes)? {
            let id = file.path.file_stem().unwrap_or_default().to_string_lossy();
            titles.insert((file.project.clone(), JsString::from(id.as_ref())), title);
        }
    }
    for file in files {
        let bytes = read_session_bytes(&file.path).map_err(|error| {
            SearchError::Error(
                crate::node_fs_message(&error, "open", Some(file.path.to_string_lossy().as_ref()))
                    .unwrap_or_else(|| error.to_string()),
            )
        })?;
        let source = String::from_utf8_lossy(&bytes);
        let mut file_matches = vec![];
        // Node readline treats CR, LF and CRLF as line boundaries.
        for (index, line) in physical_lines(&source).enumerate() {
            let Ok(entry) = parse_json(line) else {
                continue;
            };
            if matches!(entry, JsValue::Null) {
                return Err(SearchError::Error(
                    "Cannot read properties of null (reading 'sessionId')".into(),
                ));
            }
            let Some((entry_role, text)) = searchable(&entry) else {
                continue;
            };
            if role != "all" && entry_role != role {
                continue;
            }
            if matcher.find(&text)?.is_none() {
                continue;
            }
            let normalized = normalize(&text);
            let center = matcher.find(&normalized)?.unwrap_or(0);
            let units = normalized.0.units();
            let start = center.saturating_sub(60);
            let end = (center + 60).min(units.len());
            let mut snippet = Vec::new();
            if start > 0 {
                snippet.extend("...".encode_utf16());
            }
            // JS slice clamps both bounds and returns empty when start >= end.
            if start < end {
                snippet.extend_from_slice(&units[start..end]);
            }
            if end < units.len() {
                snippet.extend("...".encode_utf16());
            }
            let snippet = Text(JsString::from_utf16(snippet));
            let session_id = string(&entry, "sessionId")
                .unwrap_or_else(|| Text(file.path.to_string_lossy().as_ref().into()));
            let session_title = titles
                .get(&(file.project.clone(), session_id.0.clone()))
                .cloned()
                .unwrap_or_else(|| session_id.clone());
            file_matches.push(Match {
                session_id,
                role: entry_role,
                text,
                snippet,
                created_at: string(&entry, "createdAt").unwrap_or_else(|| Text("".into())),
                line_number: index + 1,
                session_title,
                project_key: file.project.clone(),
            });
        }
        // TS finishes reading a whole file before applying the result limit.
        for item in file_matches {
            result.matches.push(item);
            // The provider uses >= against the fractional limit, not Array.slice.
            if result.matches.len() as f64 >= limit {
                result.truncated = true;
                break;
            }
        }
        if result.truncated {
            break;
        }
    }
    Ok(result.into_value())
}
fn internal(id: &JsString) -> bool {
    [
        "always-on-discovery",
        "always-on-workspace",
        "always-on-report",
    ]
    .iter()
    .any(|prefix| {
        let prefix: Vec<_> = prefix.encode_utf16().collect();
        id.units()
            .strip_prefix(prefix.as_slice())
            .is_some_and(|rest| matches!(rest.first(), Some(58 | 45)))
    })
}
fn path_error(error: std::io::Error) -> SearchError {
    let (_, message) = crate::metadata::process_cwd_diagnostic(&error);
    SearchError::Error(message)
}
fn project_base(project: &Path, cwd: Option<&PathBuf>) -> Result<PathBuf, SearchError> {
    if let Some(cwd) = cwd {
        return Ok(cwd.clone());
    }
    if project.is_absolute() {
        Ok(project.to_path_buf())
    } else {
        std::env::current_dir().map_err(path_error)
    }
}
fn collect(options: &SearchOptions) -> Result<Vec<Target>, SearchError> {
    if let Some(id) = options.session_id.as_ref().filter(|id| !id.is_empty()) {
        if internal(id) {
            return Ok(vec![]);
        }
        let project = match &options.project_root {
            Some(project) => project.clone(),
            None => match &options.cwd {
                Some(cwd) => cwd.clone(),
                None => std::env::current_dir().map_err(path_error)?,
            },
        };
        let cwd = project_base(&project, options.cwd.as_ref())?;
        let project = project.to_string_lossy().into_owned();
        let id = crate::storage::sanitize_session_id(id);
        return Ok(vec![Target {
            path: session_chat_dir(&options.pilot_home, &cwd, &project).join(format!("{id}.jsonl")),
            project,
        }]);
    }
    if let Some(project) = options
        .project_root
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
    {
        let cwd = project_base(project, options.cwd.as_ref())?;
        let project = project.to_string_lossy();
        return Ok(list(
            &session_chat_dir(&options.pilot_home, &cwd, &project),
            &project,
        ));
    }
    Ok(list_session_entries(&options.pilot_home.join("projects"))
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(id, _)| {
            list(
                &options.pilot_home.join("projects").join(&id).join("chats"),
                &id,
            )
        })
        .collect())
}
fn list(dir: &Path, project: &str) -> Vec<Target> {
    list_session_entries(dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, _)| {
            let id = name.strip_suffix(".jsonl")?;
            (!internal(&id.into())).then(|| Target {
                path: dir.join(&name),
                project: project.into(),
            })
        })
        .collect()
}
fn string(entry: &JsValue, key: &str) -> Option<Text> {
    entry.get(key)?.as_string().map(|s| Text(s.clone()))
}
fn canonical(messages: &[JsValue]) -> Option<Text> {
    let mut parts = vec![];
    for message in messages {
        let Some(JsValue::Array(content)) = message.get("content") else {
            continue;
        };
        for block in content {
            if !string(block, "type").is_some_and(|s| s.0.equals_str("text")) {
                continue;
            }
            if let Some(text) = string(block, "text") {
                let text = trim(&text);
                if !text.0.is_empty() {
                    parts.push(text);
                }
            }
        }
    }
    (!parts.is_empty()).then(|| {
        let mut units = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                units.push(10);
            }
            units.extend_from_slice(part.0.units());
        }
        Text(JsString::from_utf16(units))
    })
}
fn searchable(entry: &JsValue) -> Option<(&'static str, Text)> {
    match string(entry, "type").map(|s| s.to_string()).as_deref() {
        Some("accepted_input") => {
            let JsValue::Array(messages) = entry.get("messages")? else {
                return None;
            };
            canonical(messages).map(|text| ("user", text))
        }
        Some("assistant_message" | "durable_message") => {
            let message = entry.get("message")?;
            message.as_object()?;
            canonical(std::slice::from_ref(message)).map(|text| ("assistant", text))
        }
        _ => None,
    }
}
fn physical_lines(source: &str) -> impl Iterator<Item = &str> {
    // split_inclusive keeps adjacent empty lines while consuming CRLF once.
    source.split_inclusive('\n').flat_map(|line| {
        line.strip_suffix('\n')
            .unwrap_or(line)
            .strip_suffix('\r')
            .unwrap_or(line.strip_suffix('\n').unwrap_or(line))
            .split('\r')
    })
}
fn space(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(text: &Text) -> Text {
    let units = text.0.units();
    let start = units.iter().position(|u| !space(*u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !space(*u))
        .map_or(start, |i| i + 1);
    Text(JsString::from_utf16(units[start..end].to_vec()))
}
fn normalize(text: &Text) -> Text {
    let mut units = Vec::new();
    for part in text
        .0
        .units()
        .split(|u| space(*u))
        .filter(|s| !s.is_empty())
    {
        if !units.is_empty() {
            units.push(32);
        }
        units.extend_from_slice(part);
    }
    Text(JsString::from_utf16(units))
}
