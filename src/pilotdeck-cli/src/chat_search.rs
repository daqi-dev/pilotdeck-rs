//! Native JSONL search. Behavior follows cli/commands/chatSearch.ts and
//! session/search/NodeSessionSearchPort.ts, including physical line numbers.
use crate::{
    Failure,
    paths::{Env, js_trim, node_fs_message, pilot_home, resolve},
    stdout_line,
};
use pilotdeck_gateway::{list_session_entries, read_session_bytes, session_chat_dir};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
mod matcher;
mod title;

#[derive(Clone)]
struct Text(JsString);
impl Serialize for Text {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let json = stringify_json(&JsValue::String(self.0.clone()))
            .unwrap()
            .unwrap();
        serde_json::value::RawValue::from_string(json)
            .unwrap()
            .serialize(serializer)
    }
}
impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf16_lossy(self.0.units()))
    }
}
use matcher::Matcher;

const USAGE: &str = "Usage: pilotdeck chat search <keyword> [--project <path>] [--all-projects] [--limit N] [--json] [--regex] [--case-sensitive] [--role user|assistant|all] [--session <id>]";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResult {
    query: String,
    matches: Vec<Match>,
    truncated: bool,
    sessions_scanned: usize,
}
struct Target {
    path: PathBuf,
    project: String,
}

pub fn run(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    if argv.first().map(String::as_str) != Some("search") {
        return Err(Failure::Error(USAGE.into()));
    }
    let all = &argv[1..];
    let query = all
        .iter()
        .enumerate()
        .filter(|(index, token)| {
            !token.starts_with("--")
                && (*index == 0
                    || !matches!(
                        all[index - 1].as_str(),
                        "--project" | "--limit" | "--role" | "--session"
                    ))
        })
        .map(|(_, token)| token.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let query = js_trim(&query).to_owned();
    if query.is_empty() {
        return Err(Failure::Error("Error: search keyword is required.".into()));
    }
    let flag = |name: &str| {
        argv.iter()
            .position(|arg| arg == name)
            .and_then(|i| argv.get(i + 1))
            .map(String::as_str)
    };
    let has = |name: &str| argv.iter().any(|arg| arg == name);
    let home = match flag("--pilot-home") {
        Some(path) => resolve(cwd, Path::new(path)),
        None => pilot_home(env, cwd)?,
    };
    let all_projects = has("--all-projects");
    let project = flag("--project").unwrap_or_else(|| cwd.to_str().unwrap_or(""));
    let limit = flag("--limit")
        .filter(|value| !value.is_empty())
        .and_then(number)
        .filter(|n| n.is_finite())
        .unwrap_or(20.0)
        .clamp(1.0, 100.0);
    let role = flag("--role")
        .filter(|role| matches!(*role, "user" | "assistant"))
        .unwrap_or("all");
    let matcher = Matcher::new(&query, has("--regex"), has("--case-sensitive"))?;
    let files = collect(
        &home,
        cwd,
        (!all_projects).then_some(project),
        flag("--session"),
    );
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
            Failure::Error(
                node_fs_message(&error, "open", &file.path).unwrap_or_else(|| error.to_string()),
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
                return Err(Failure::Error(
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
    if has("--json") {
        stdout_line(
            &serde_json::to_string_pretty(&result).map_err(|e| Failure::Error(e.to_string()))?,
        );
    } else {
        stdout_line(&format_result(&result, all_projects, env));
    }
    Ok(0)
}

fn number(text: &str) -> Option<f64> {
    let text = js_trim(text);
    if text.is_empty() {
        return Some(0.0);
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(digits) = text.strip_prefix(prefix) {
            if digits.is_empty() {
                return None;
            }
            return digits.chars().try_fold(0.0, |sum, c| {
                c.to_digit(radix)
                    .map(|n| sum * f64::from(radix) + f64::from(n))
            });
        }
    }
    text.parse().ok()
}
fn internal(id: &str) -> bool {
    [
        "always-on-discovery",
        "always-on-workspace",
        "always-on-report",
    ]
    .iter()
    .any(|prefix| {
        id.strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with([':', '-']))
    })
}
fn collect(home: &Path, cwd: &Path, project: Option<&str>, session: Option<&str>) -> Vec<Target> {
    if let Some(id) = session.filter(|id| !id.is_empty()) {
        if internal(id) {
            return vec![];
        }
        let project = project.unwrap_or_else(|| cwd.to_str().unwrap_or(""));
        let id = pilotdeck_gateway::storage::sanitize_session_id(&id.into());
        return vec![Target {
            path: session_chat_dir(home, cwd, project).join(format!("{id}.jsonl")),
            project: project.into(),
        }];
    }
    if let Some(project) = project.filter(|p| !p.is_empty()) {
        return list(&session_chat_dir(home, cwd, project), project);
    }
    list_session_entries(&home.join("projects"))
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(id, _)| list(&home.join("projects").join(&id).join("chats"), &id))
        .collect()
}
fn list(dir: &Path, project: &str) -> Vec<Target> {
    list_session_entries(dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, _)| {
            let id = name.strip_suffix(".jsonl")?;
            (!internal(id)).then(|| Target {
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
    char::from_u32(u32::from(unit)).is_some_and(|c| js_trim(&c.to_string()).is_empty())
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

fn format_result(result: &SearchResult, projects: bool, env: &Env) -> String {
    if result.matches.is_empty() {
        return format!(
            "No chat history matches for \"{}\".\nScanned {} session(s).",
            result.query, result.sessions_scanned
        );
    }
    let mut lines = vec![
        format!(
            "🔍 {} match(es) for \"{}\"{}",
            result.matches.len(),
            result.query,
            if result.truncated {
                " (truncated — try a higher --limit)"
            } else {
                ""
            }
        ),
        String::new(),
    ];
    for (index, item) in result.matches.iter().enumerate() {
        let units = item.session_id.0.units();
        let short = if units.len() > 12 {
            format!("{}…", String::from_utf16_lossy(&units[..8]))
        } else {
            item.session_id.to_string()
        };
        let project = if projects && !item.project_key.is_empty() {
            format!(
                " · {}",
                item.project_key
                    .rsplit(['/', '\\'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&item.project_key)
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "{}. **{}** (`{short}`){project}\n   {} · {}\n   > {}",
            index + 1,
            item.session_title,
            item.role,
            when(&item.created_at.to_string(), env),
            item.snippet
        ));
        lines.push(String::new());
    }
    lines.push("Tip: open the session in the Web UI to jump to the matched message.".into());
    lines.join("\n")
}
fn when(text: &str, _env: &Env) -> String {
    if text.is_empty() {
        return "unknown time".into();
    }
    use chrono::{Datelike, TimeZone, Timelike};
    use pilotdeck_transport::system_clock::parse_node22_date;
    let Ok(Some(ms)) = parse_node22_date(&text.into()) else {
        return text.into();
    };
    let Some(parts) = chrono::Local.timestamp_millis_opt(ms as i64).single() else {
        return text.into();
    };
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{} {}, {:02}:{:02} {}",
        months[parts.month() as usize - 1],
        parts.day(),
        if parts.hour() % 12 == 0 {
            12
        } else {
            parts.hour() % 12
        },
        parts.minute(),
        if parts.hour() < 12 { "AM" } else { "PM" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory named like a session file is the reachable read-stage
    /// failure: `list` filters only the `.jsonl` suffix, and the whole-file
    /// read behind `read_session_bytes` already stages its errors. The TS
    /// provider reads the same entry through `createReadStream`
    /// (NodeSessionSearchPort.ts:189), whose directory error is
    /// `EISDIR: illegal operation on a directory, read` with no path (node
    /// v22.22.0, `_tmp/20261025-issue-651/node-probe-stream.out`); the CLI
    /// must not report the caller's `open` label with the path.
    #[test]
    fn a_directory_named_like_a_session_reports_the_read_stage() {
        let home = std::env::temp_dir().join(format!(
            "pdrs-chat-eisdir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let chats = home.join("projects/p/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::create_dir(chats.join("dir.jsonl")).unwrap();
        let env = crate::paths::Env::from_pairs(&[("PILOT_HOME", home.to_string_lossy().as_ref())]);
        let error = run(
            &[
                "search".to_string(),
                "needle".to_string(),
                "--all-projects".to_string(),
            ],
            &env,
            &chats,
        )
        .expect_err("a directory entry must fail the search");
        let _ = std::fs::remove_dir_all(&home);
        match error {
            Failure::Error(message) => {
                assert_eq!(message, "EISDIR: illegal operation on a directory, read")
            }
            other => panic!("expected Failure::Error, got {other:?}"),
        }
    }
}
