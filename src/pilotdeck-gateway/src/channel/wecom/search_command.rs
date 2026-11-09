//! /search syntax and Chinese presentation shared with the TS IM registry.
use super::{delivery, transport::ActiveTransport};
use crate::channel::protocol::ChannelSearchPort;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
const USAGE: &str =
    "用法：/search <关键词> [--all] [--limit N] [--role user|assistant]\n示例：/search docker 部署";
fn text(value: Option<&JsValue>) -> String {
    value
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_default()
}
fn put(obj: &mut JsObject, key: &str, value: String) {
    obj.insert(key.into(), JsValue::String(value.into()));
}
fn when(value: &str) -> String {
    if value.is_empty() {
        return "未知时间".into();
    }
    match chrono::DateTime::parse_from_rfc3339(value) {
        Ok(date) => {
            let date = date.with_timezone(&chrono::Local);
            date.format("%-m月%-d日 %H:%M").to_string()
        }
        Err(_) => value.to_string(),
    }
}
fn format_result(result: &JsValue, include_project: bool) -> String {
    let query = text(result.get("query"));
    if query.trim().is_empty() {
        return USAGE.into();
    }
    let Some(JsValue::Array(matches)) = result.get("matches") else {
        return "命令执行失败: 无效的搜索结果".into();
    };
    let count = match result.get("sessionsScanned") {
        Some(JsValue::Number(n)) => *n as usize,
        _ => 0,
    };
    if matches.is_empty() {
        return format!("未找到包含「{query}」的聊天记录。\n已扫描 {count} 个会话。");
    }
    let truncated = result.get("truncated") == Some(&JsValue::Bool(true));
    let mut lines = vec![
        format!(
            "🔍 找到 {} 条匹配「{}」{}",
            matches.len(),
            query,
            if truncated {
                "（结果已截断，可加大 --limit）"
            } else {
                ""
            }
        ),
        String::new(),
    ];
    for (index, item) in matches.iter().enumerate() {
        let title = text(item.get("sessionTitle"));
        let session = item
            .get("sessionId")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let id = if session.units().len() <= 12 {
            String::from_utf16_lossy(session.units())
        } else {
            format!("{}…", String::from_utf16_lossy(&session.units()[..8]))
        };
        let project = text(item.get("projectKey"));
        let suffix = if include_project && !project.is_empty() {
            format!(
                " · {}",
                project
                    .rsplit(['/', '\\'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&project)
            )
        } else {
            String::new()
        };
        let role = if text(item.get("role")) == "user" {
            "用户"
        } else {
            "助手"
        };
        lines.push(format!(
            "{}. **{}** (`{}`){}\n   {} · {}\n   > {}",
            index + 1,
            title,
            id,
            suffix,
            role,
            when(&text(item.get("createdAt"))),
            text(item.get("snippet"))
        ));
        lines.push(String::new());
    }
    lines.push("提示：在 Web UI 中打开对应会话后，可用 /search 结果中的片段定位消息。".into());
    lines.join("\n").trim_end().to_string()
}
pub(crate) fn matches(text: &str) -> bool {
    let Some(command) = text.trim().strip_prefix("/") else {
        return false;
    };
    let name = command
        .split_once(" ")
        .map(|(name, _)| name)
        .unwrap_or(command)
        .to_lowercase();
    matches!(name.as_str(), "search" | "find" | "grep" | "搜索")
}
pub(crate) async fn execute(
    text_input: &str,
    project: Option<JsString>,
    pilot_home: Option<&str>,
    search: Option<&dyn ChannelSearchPort>,
    transport: &ActiveTransport,
    request_id: &delivery::ReplyTarget,
) -> bool {
    let Some(command) = text_input.trim().strip_prefix("/") else {
        return false;
    };
    let (name, arg) = command
        .split_once(" ")
        .map(|(n, a)| (n.to_lowercase(), a.trim()))
        .unwrap_or_else(|| (command.to_lowercase(), ""));
    if !matches!(name.as_str(), "search" | "find" | "grep" | "搜索") {
        return false;
    }
    if arg.is_empty() {
        delivery::reply(transport, request_id.clone(), USAGE.into()).await;
        return true;
    }
    let (Some(search), Some(home)) = (search, pilot_home) else {
        delivery::reply(
            transport,
            request_id.clone(),
            "聊天记录搜索当前不可用。".into(),
        )
        .await;
        return true;
    };
    let mut params = JsObject::new();
    put(&mut params, "pilotHome", home.into());
    params.insert("regex".into(), JsValue::Bool(false));
    params.insert("caseSensitive".into(), JsValue::Bool(false));
    let tokens: Vec<&str> = arg.split_whitespace().collect();
    let mut i = 0;
    let mut terms = Vec::new();
    let mut all = false;
    while i < tokens.len() {
        match tokens[i] {
            "--all" | "-a" => all = true,
            "--regex" | "-E" => {
                params.insert("regex".into(), JsValue::Bool(true));
            }
            "--case-sensitive" => {
                params.insert("caseSensitive".into(), JsValue::Bool(true));
            }
            "--limit" | "-n" => {
                if let Some(n) = tokens
                    .get(i + 1)
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|n| n.is_finite() && *n > 0.0)
                {
                    params.insert("limit".into(), JsValue::Number(n.floor().min(100.0)));
                    i += 1;
                }
            }
            "--role" | "-r" => {
                if let Some(v) = tokens
                    .get(i + 1)
                    .filter(|v| matches!(**v, "user" | "assistant" | "all"))
                {
                    put(&mut params, "role", (*v).into());
                    i += 1;
                }
            }
            "--session" | "-s" => {
                if let Some(v) = tokens.get(i + 1) {
                    put(&mut params, "sessionId", (*v).into());
                    i += 1;
                }
            }
            token => terms.push(token),
        }
        i += 1;
    }
    put(&mut params, "query", terms.join(" "));
    let include_project = all || project.is_none();
    if !all && let Some(project) = project {
        params.insert("projectRoot".into(), JsValue::String(project));
    }
    let answer = match search.search(JsValue::Object(params)).await {
        Ok(result) => format_result(&result, include_project),
        Err(error) => format!("命令执行失败: {}", error.message()),
    };
    delivery::reply(transport, request_id.clone(), answer.into()).await;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_result_matches_fixed_ts_formatter() {
        let result = pilotdeck_protocol::parse_json(r#"{"query":"abc","matches":[{"sessionTitle":"Test","sessionId":"123456789012345","role":"user","createdAt":"","snippet":"alpha","projectKey":"/x/demo"}],"sessionsScanned":1,"truncated":false}"#).unwrap();
        assert_eq!(
            format_result(&result, true),
            "🔍 找到 1 条匹配「abc」\n\n1. **Test** (`12345678…`) · demo\n   用户 · 未知时间\n   > alpha\n\n提示：在 Web UI 中打开对应会话后，可用 /search 结果中的片段定位消息。"
        );
    }
}
