//! Shared gateway project commands scoped by the WeCom session mapper.
use super::{delivery, mapper::WeComSessionMapper, transport::ActiveTransport};
use crate::channel::protocol::ChannelControlPort;
use pilotdeck_protocol::{JsObject, JsValue};

pub(crate) async fn execute(
    text: &str,
    scope: &JsValue,
    mapper: &mut WeComSessionMapper,
    control: &dyn ChannelControlPort,
    transport: &ActiveTransport,
    request_id: &delivery::ReplyTarget,
) -> bool {
    let Some(command) = text.trim().strip_prefix("/") else {
        return false;
    };
    let (name, arg) = command
        .split_once(" ")
        .map(|(n, a)| (n.to_lowercase(), a.trim()))
        .unwrap_or_else(|| (command.to_lowercase(), ""));
    if !matches!(
        name.as_str(),
        "projects" | "项目列表" | "switch-project" | "切换项目"
    ) {
        return false;
    }
    if matches!(name.as_str(), "switch-project" | "切换项目") && arg.is_empty() {
        delivery::reply(
            transport,
            request_id.clone(),
            "用法：/switch-project <项目名>\n\n发送 /projects 查看可用项目。".into(),
        )
        .await;
        return true;
    }
    let response = match control
        .call("list_projects", JsValue::Object(JsObject::new()))
        .await
    {
        Ok(value) => value,
        Err(error) => {
            delivery::reply(
                transport,
                request_id.clone(),
                format!("命令执行失败: {}", error.message()).into(),
            )
            .await;
            return true;
        }
    };
    let Some(JsValue::Array(projects)) = response.get("projects") else {
        delivery::reply(
            transport,
            request_id.clone(),
            "命令执行失败: 无效的项目列表".into(),
        )
        .await;
        return true;
    };
    let field = |project: &JsValue, name: &str| {
        project
            .get(name)
            .and_then(JsValue::as_string)
            .map(|s| String::from_utf16_lossy(s.units()))
            .unwrap_or_default()
    };
    if matches!(name.as_str(), "projects" | "项目列表") {
        let answer = if projects.is_empty() {
            "暂无项目。使用 Web UI 创建 WorkSpace 后即可在此切换。".to_string()
        } else {
            let current = mapper.get_project(scope).ok().flatten();
            let mut lines = vec!["📂 项目列表：".to_string(), String::new()];
            for project in projects {
                let marker =
                    if project.get("projectKey").and_then(JsValue::as_string) == current.as_ref() {
                        " ✅"
                    } else {
                        ""
                    };
                lines.push(format!("• {}{}", field(project, "name"), marker));
            }
            lines.push(String::new());
            lines.push("发送 /switch-project <项目名> 切换 WorkSpace".into());
            lines.join("\n")
        };
        delivery::reply(transport, request_id.clone(), answer.into()).await;
        return true;
    }
    let lower = arg.to_lowercase();
    let target = projects
        .iter()
        .find(|p| field(p, "name") == arg)
        .or_else(|| {
            projects
                .iter()
                .find(|p| field(p, "name").to_lowercase() == lower)
        })
        .or_else(|| {
            projects
                .iter()
                .find(|p| field(p, "name").to_lowercase().contains(&lower))
        });
    let answer = match target {
        None => format!("未找到匹配「{arg}」的项目。\n\n发送 /projects 查看可用项目。"),
        Some(project) => {
            let key = project
                .get("projectKey")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default();
            if mapper.bind_project(scope, key).is_ok() {
                let mut reset = scope.clone();
                if let JsValue::Object(ref mut obj) = reset {
                    obj.insert("text".into(), "/new".into());
                }
                let _ = mapper.resolve(&reset, &uuid::Uuid::new_v4().to_string().into());
            }
            format!(
                "已切换到项目：{}\n路径：{}\n（已自动创建新会话）",
                field(project, "name"),
                field(project, "fullPath")
            )
        }
    };
    delivery::reply(transport, request_id.clone(), answer.into()).await;
    true
}
