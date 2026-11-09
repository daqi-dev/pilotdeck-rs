//! Native JSONL search. Behavior follows cli/commands/chatSearch.ts and
//! session/search/NodeSessionSearchPort.ts, including physical line numbers.
use crate::{
    Failure,
    paths::{Env, js_trim, pilot_home, resolve},
    stdout_line,
};
use pilotdeck_gateway::runtime::history::search::{SearchError, SearchOptions, search};
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
use serde::Serialize;
use std::path::{Path, PathBuf};

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
        None => resolve(cwd, &pilot_home(env, cwd)?),
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
    let value = search(SearchOptions {
        cwd: Some(cwd.to_path_buf()),
        pilot_home: home,
        project_root: (!all_projects).then(|| PathBuf::from(project)),
        query: query.as_str().into(),
        limit,
        regex: has("--regex"),
        case_sensitive: has("--case-sensitive"),
        role: role.into(),
        session_id: flag("--session").map(Into::into),
    })
    .map_err(|error| match error {
        SearchError::Error(message) => Failure::Error(message),
        SearchError::Message(message) => Failure::Error(String::from_utf16_lossy(message.units())),
    })?;
    let text = |v: &JsValue, key| {
        Text(
            v.get(key)
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default(),
        )
    };
    let entries = match value.get("matches") {
        Some(JsValue::Array(entries)) => entries.as_slice(),
        _ => &[],
    };
    let result = SearchResult {
        query,
        matches: entries
            .iter()
            .map(|entry| Match {
                session_id: text(entry, "sessionId"),
                role: if entry
                    .get("role")
                    .and_then(JsValue::as_string)
                    .is_some_and(|v| v.equals_str("user"))
                {
                    "user"
                } else {
                    "assistant"
                },
                text: text(entry, "text"),
                snippet: text(entry, "snippet"),
                created_at: text(entry, "createdAt"),
                line_number: match entry.get("lineNumber") {
                    Some(JsValue::Number(n)) => n.to_owned() as usize,
                    _ => 0,
                },
                session_title: text(entry, "sessionTitle"),
                project_key: text(entry, "projectKey").to_string(),
            })
            .collect(),
        truncated: value.get("truncated") == Some(&JsValue::Bool(true)),
        sessions_scanned: match value.get("sessionsScanned") {
            Some(JsValue::Number(n)) => n.to_owned() as usize,
            _ => 0,
        },
    };
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
    /// must not report the caller's `open` label with the path. POSIX-only:
    /// opening a directory read-only succeeds there, unlike Windows.
    #[cfg(unix)]
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
