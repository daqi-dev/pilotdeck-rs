//! Read a child transcript selected by the first durable parent reference.
use super::{
    budget::Budget,
    entries, events,
    project::{Context, ProjectionError, flatten_message_budgeted},
    reader::{HistoryFiles, READ_SLOTS, background_path, history_path},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(name).unwrap_or(&UNDEFINED)
}
fn is(value: &JsValue, expected: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(expected))
}
fn relative_path(root: &Path, base: &Path, value: &JsString) -> Result<PathBuf, String> {
    let text = value
        .to_utf8()
        .map_err(|_| "relativeTranscriptPath is not valid UTF-8".to_owned())?;
    let input = Path::new(&text);
    if text.is_empty() || input.is_absolute() {
        return Err("relativeTranscriptPath must be a relative path.".into());
    }
    let mut relative = base
        .strip_prefix(root)
        .map_err(|_| {
            "relativeTranscriptPath points outside the project transcript directory.".to_owned()
        })?
        .to_path_buf();
    for part in input.components() {
        match part {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => (),
            Component::ParentDir if relative.pop() => (),
            _ => {
                return Err(
                    "relativeTranscriptPath points outside the project transcript directory."
                        .into(),
                );
            }
        }
    }
    let name = relative.to_string_lossy();
    if name.is_empty() || name.starts_with("..") || !name.ends_with(".jsonl") {
        return Err(
            "relativeTranscriptPath points outside the project transcript directory.".into(),
        );
    }
    Ok(root.join(relative))
}

/// Sidechain replay is execution-only: the accepted_input fork prelude is not output.
pub(super) fn project(
    entries_in: &[JsValue],
    input: &JsValue,
    now: &JsValue,
) -> Result<JsValue, ProjectionError> {
    let session = field(input, "sessionKey")
        .as_string()
        .ok_or_else(|| ProjectionError("History sessionKey must be a string".into()))?;
    let child = field(input, "subagentId")
        .as_string()
        .ok_or_else(|| ProjectionError("History subagentId must be a string".into()))?;
    let mut key = session.units().to_vec();
    key.extend("::sub::".encode_utf16());
    key.extend_from_slice(child.units());
    let key = JsString::from_utf16(key);
    let mut output = Vec::new();
    let mut compact = Vec::new();
    let mut budget = Budget::default();
    let mut index = 0;
    let mut saw_execution = false;
    for entry in entries_in {
        let typ = field(entry, "type");
        if [
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .iter()
        .any(|x| is(typ, x))
        {
            saw_execution = true;
            let message = field(entry, "message");
            if !entries::visible(message) {
                continue;
            }
            let ctx = Context {
                session_key: &key,
                project_key: input.get("projectKey"),
                created_at: entry.get("createdAt").unwrap_or(now),
                entry_id: entry.get("entryId"),
                turn_id: entry.get("turnId"),
                sequence: entry.get("sequence"),
                message_index: index,
                fork_unsupported: false,
            };
            output.extend(flatten_message_budgeted(message, &ctx, &mut budget)?);
            index += 1;
        } else if saw_execution
            && is(typ, "control_boundary")
            && is(field(field(entry, "boundary"), "kind"), "compact")
        {
            compact.push(entry);
        }
    }
    for entry in compact {
        events::inject_compact(
            entry,
            &mut output,
            &key,
            input.get("projectKey"),
            &mut budget,
        )?;
    }
    let total = output.len();
    let mut result = JsObject::new();
    result.insert("messages".into(), JsValue::Array(output));
    result.insert("total".into(), JsValue::Number(total as f64));
    Ok(JsValue::Object(result))
}

pub async fn read(files: HistoryFiles, input: JsValue) -> Result<JsValue, String> {
    let session = field(&input, "sessionKey")
        .as_string()
        .cloned()
        .ok_or("History sessionKey must be a string")?;
    let subagent = field(&input, "subagentId")
        .as_string()
        .cloned()
        .ok_or("History subagentId must be a string")?;
    let parent = input
        .get("parentSessionId")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .map(|v| {
            v.as_string()
                .cloned()
                .ok_or("History parentSessionId must be a string")
        })
        .transpose()?
        .unwrap_or(session);
    let permit = READ_SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let normal_parent = history_path(&files, &parent)?;
        let chat = normal_parent.parent().ok_or("Invalid history directory")?;
        // Default CLI uses a storage provider. Background tasks bypass that
        // provider and resolve their parent path from the request instead.
        let background = background_path(&input, chat)?;
        let parent_path = background.as_ref().unwrap_or(&normal_parent);
        let parent_data = if background.is_some() {
            crate::storage::read_transcript_beneath(chat, parent_path)
        } else {
            crate::storage::read_transcript_bounded(parent_path)
        }
        .map_err(|e| e.to_string())?;
        // Find the first reference without cloning an entire quadratic index.
        let reference = parent_data.read.entries.iter().find(|e| {
            is(field(e, "type"), "subagent_started")
                && field(e, "subagentId").as_string() == Some(&subagent)
        });
        let relative = reference
            .and_then(|e| field(e, "transcriptRelativePath").as_string())
            .filter(|v| !v.units().is_empty())
            .cloned();
        let provider_child = reference
            .and_then(|e| field(e, "subagentSessionId").as_string())
            .is_some_and(|v| !v.units().is_empty());
        drop(parent_data);
        let path = if background.is_none() && provider_child {
            chat.join(crate::storage::sanitize_session_id(&parent))
                .join("subagents")
                .join(format!(
                    "{}.jsonl",
                    crate::storage::sanitize_session_id(&subagent)
                ))
        } else if let Some(relative) = relative {
            relative_path(
                chat,
                parent_path.parent().ok_or("Invalid history directory")?,
                &relative,
            )?
        } else {
            return project(&[], &input, &JsValue::Undefined).map_err(|e| e.to_string());
        };
        let child =
            crate::storage::read_transcript_beneath(chat, &path).map_err(|e| e.to_string())?;
        let now = JsValue::from(
            crate::storage::transcript_timestamp_for_history()
                .map_err(|e| e.to_string())?
                .as_str(),
        );
        project(&child.read.entries, &input, &now).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{parse_json, stringify_json};
    #[test]
    fn bundle_execution_projection_matches_ts() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-history/subagent-bundle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = field(&oracle, "cases") else {
            panic!("cases")
        };
        for case in cases {
            let id = field(case, "id").as_string().unwrap().to_utf8().unwrap();
            let Some(expected) = case.get("result") else {
                continue;
            };
            let JsValue::Array(child) = field(case, "child") else {
                panic!("child")
            };
            let entries =
                if ["missing-reference", "unknown-id", "missing-child"].contains(&id.as_str()) {
                    &[][..]
                } else {
                    child.as_slice()
                };
            let now = JsValue::from("2026-01-02T03:04:05.678Z");
            let actual = project(entries, field(case, "input"), &now).unwrap();
            let actual = parse_json(&stringify_json(&actual).unwrap().unwrap()).unwrap();
            assert_eq!(&actual, expected, "{id}");
        }
    }
    #[test]
    fn provider_projection_matches_real_ts() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-history/subagent-provider.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = field(&oracle, "cases") else {
            panic!("cases")
        };
        let child = parse_json(r#"[{"type":"assistant_message","sessionId":"parent","turnId":"t","sequence":1,"createdAt":"2026-01-02T03:04:05.678Z","entryId":"e1","message":{"role":"assistant","content":[{"type":"text","text":"PROVIDER CHILD"}]}}]"#).unwrap();
        let JsValue::Array(rows) = &child else {
            panic!("child")
        };
        for case in cases {
            let Some(expected) = case.get("result") else {
                continue;
            };
            let got = project(
                rows,
                field(case, "input"),
                &JsValue::from("2026-01-02T03:04:05.678Z"),
            )
            .unwrap();
            let got = parse_json(&stringify_json(&got).unwrap().unwrap()).unwrap();
            assert_eq!(&got, expected, "{:?}", field(case, "id"));
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn provider_and_background_choose_real_parent_and_child_files() {
        use std::fs;
        let root = std::env::temp_dir().join(format!("pdrs-subagent-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let files = HistoryFiles {
            home: root.join("home"),
            project: root.join("project"),
        };
        fs::create_dir_all(&files.project).unwrap();
        let parent = history_path(&files, &JsString::from("p")).unwrap();
        let chat = parent.parent().unwrap();
        fs::create_dir_all(chat.join("p/subagents")).unwrap();
        fs::create_dir_all(chat.join("tasks")).unwrap();
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-history/subagent-bundle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = field(&oracle, "cases") else {
            panic!("cases")
        };
        let encode = |v: &JsValue| {
            let JsValue::Array(rows) = v else {
                panic!("rows")
            };
            rows.iter()
                .map(|row| format!("{}\n", stringify_json(row).unwrap().unwrap()))
                .collect::<String>()
        };
        let find = |id: &str| cases.iter().find(|v| is(field(v, "id"), id)).unwrap();
        let normal = find("child");
        fs::write(&parent, encode(field(normal, "parent"))).unwrap();
        fs::write(
            chat.join("p/subagents/child.jsonl"),
            encode(field(normal, "child")),
        )
        .unwrap();
        let got = read(files.clone(), field(normal, "input").clone())
            .await
            .unwrap();
        let got = parse_json(&stringify_json(&got).unwrap().unwrap()).unwrap();
        assert_eq!(&got, field(normal, "result"));
        let bg = find("background");
        fs::write(chat.join("tasks/parent.jsonl"), encode(field(bg, "parent"))).unwrap();
        fs::write(chat.join("tasks/child.jsonl"), encode(field(bg, "child"))).unwrap();
        let got = read(files.clone(), field(bg, "input").clone())
            .await
            .unwrap();
        let got = parse_json(&stringify_json(&got).unwrap().unwrap()).unwrap();
        assert_eq!(&got, field(bg, "result"));
        let unknown = parse_json(r#"{"sessionKey":"p","subagentId":"unknown"}"#).unwrap();
        assert_eq!(
            field(&read(files.clone(), unknown).await.unwrap(), "total"),
            &JsValue::Number(0.0)
        );
        let normal_child = chat.join("p/subagents/child.jsonl");
        fs::remove_file(&normal_child).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&normal_child)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            read(files.clone(), field(normal, "input").clone())
                .await
                .is_err()
        );
        let invalid = find("invalid-child-path");
        let parent_text =
            encode(field(invalid, "parent")).replace(",\"subagentSessionId\":\"sub-session\"", "");
        assert!(!parent_text.contains("subagentSessionId"));
        fs::write(&parent, parent_text).unwrap();
        assert!(
            read(files, field(invalid, "input").clone())
                .await
                .unwrap_err()
                .contains("outside")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn child_path_is_relative_to_parent_and_cannot_escape() {
        let root = Path::new("/tmp/chat");
        let base = root.join("tasks");
        assert_eq!(
            relative_path(root, &base, &JsString::from("../sub/child.jsonl")).unwrap(),
            root.join("sub/child.jsonl")
        );
        for path in ["../../outside.jsonl", "/outside.jsonl", "child.txt"] {
            assert!(relative_path(root, &base, &JsString::from(path)).is_err());
        }
    }
}
