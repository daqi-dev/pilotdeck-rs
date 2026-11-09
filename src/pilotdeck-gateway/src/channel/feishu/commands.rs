//! Fixed cc7c4e6 centralized command contract over shared host capabilities.
use super::Runtime;
use crate::channel::protocol::{
    ChannelControlPort, ChannelError, ChannelSearchPort, ChannelStatusPort, ChannelUpdatePort,
};
use pilotdeck_protocol::{JsValue, parse_json};
use std::{path::PathBuf, sync::Arc};
#[derive(Clone, Default)]
pub struct FeishuHostPorts {
    pub control: Option<Arc<dyn ChannelControlPort>>,
    pub search: Option<Arc<dyn ChannelSearchPort>>,
    pub status: Option<Arc<dyn ChannelStatusPort>>,
    pub update: Option<Arc<dyn ChannelUpdatePort>>,
    pub pilot_home: Option<PathBuf>,
}
fn text(value: Option<&JsValue>) -> String {
    value
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_default()
}
fn js(value: serde_json::Value) -> JsValue {
    parse_json(&value.to_string()).unwrap()
}
fn failure(message: &str) -> ChannelError {
    ChannelError::error(message)
}
fn save(runtime: &Runtime) {
    if let Some(save) = &runtime.on_state_change {
        save(
            runtime
                .state
                .lock()
                .expect("feishu mapper")
                .mapper
                .snapshot_js(),
        );
    }
}
pub(crate) const HELP: &str = "📋 可用命令：\n\n/new — Create a new conversation session\n/plan — 先生成计划并请求确认\n/projects (/项目列表) — List available projects\n/switch-project (/切换项目) — Switch active project for this chat\n/update (/升级, /更新) — Install the latest supported Release and request a Web service restart\n/status (/状态) — Show PilotDeck status and version\n/search (/find, /grep, /搜索) — Search chat history across sessions\n/help (/帮助) — Show available commands";
pub(crate) fn resolve_command(input: &str) -> Option<(&'static str, &str)> {
    let command = crate::metadata::js_trim(input).strip_prefix('/')?;
    let (name, arg) = command
        .split_once(' ')
        .map(|(n, a)| (n.to_lowercase(), crate::metadata::js_trim(a)))
        .unwrap_or_else(|| (command.to_lowercase(), ""));
    let name = match name.as_str() {
        "new" => "new",
        "plan" => "plan",
        "projects" | "项目列表" => "projects",
        "switch-project" | "切换项目" => "switch-project",
        "update" | "升级" | "更新" => "update",
        "status" | "状态" => "status",
        "search" | "find" | "grep" | "搜索" => "search",
        "help" | "帮助" => "help",
        _ => return None,
    };
    Some((name, arg))
}
pub(super) async fn execute(runtime: &Arc<Runtime>, chat: &str, input: &str) -> bool {
    let Some((name, arg)) = resolve_command(input) else {
        return false;
    };
    if matches!(name, "new" | "plan") {
        return false;
    }
    let ports = runtime
        .host_ports
        .lock()
        .expect("feishu host ports")
        .clone();
    let result=async{
 match name{
 "help"=>{runtime.send(chat,HELP).await;},
 "projects"|"switch-project"=>{
 if name=="switch-project"&&arg.is_empty(){runtime.send(chat,"用法：/switch-project <项目名>\n\n发送 /projects 查看可用项目。").await;return Ok(());}
 let control=ports.control.as_ref().ok_or_else(||failure("Gateway not ready"))?;
 let value=control.call("list_projects",js(serde_json::json!({}))).await?;
 let projects=value.get("projects").and_then(|v|if let JsValue::Array(a)=v{Some(a)}else{None}).ok_or_else(||failure("Invalid project list"))?;
 if name=="projects"{
 if projects.is_empty(){runtime.send(chat,"暂无项目。使用 Web UI 创建 WorkSpace 后即可在此切换。").await;return Ok(());}
 let current=runtime.state.lock().expect("feishu mapper").mapper.get_project(chat);
 let mut lines=vec!["📂 项目列表：".to_owned(),String::new()];
 for project in projects{lines.push(format!("• {}{}",text(project.get("name")),if current.as_deref()==Some(text(project.get("projectKey")).as_str()){" ✅"}else{""}));}
 lines.push(String::new());lines.push("发送 /switch-project <项目名> 切换 WorkSpace".into());runtime.send(chat,&lines.join("\n")).await;
 }else{
 let lower=arg.to_lowercase();let target=projects.iter().find(|p|text(p.get("name"))==arg).or_else(||projects.iter().find(|p|text(p.get("name")).to_lowercase()==lower)).or_else(||projects.iter().find(|p|text(p.get("name")).to_lowercase().contains(&lower)));
 let Some(target)=target else{runtime.send(chat,&format!("未找到匹配「{arg}」的项目。\n\n发送 /projects 查看可用项目。")).await;return Ok(());};
 {let mut state=runtime.state.lock().expect("feishu mapper");state.mapper.bind_project(chat,&text(target.get("projectKey")));state.mapper.resolve(chat,"/new");}save(runtime);
 runtime.send(chat,&format!("已切换到项目：{}\n路径：{}\n（已自动创建新会话）",text(target.get("name")),text(target.get("fullPath")))).await;
 }
 },
 "status"=>{
 let status=ports.status.as_ref().ok_or_else(||failure("Status service unavailable"))?.read().await?;
 let min=(status.uptime/60.0).floor()as u64;let uptime=if min>=60{format!("{}h {}m",min/60,min%60)}else{format!("{min}m")};
 runtime.send(chat,&format!("📊 PilotDeck Status\n\n分支: {}\n提交: {}\n运行时间: {uptime}\nRuntime: {}\n平台: {}",status.branch.trim(),status.commit.trim(),status.runtime_version,status.platform)).await;
 },
 "update"=>{
 if !arg.is_empty()&&arg!="check"{runtime.send(chat,"用法：/update check 检查 Release；/update 更新并请求重启。未运行 Web 服务时，请使用 CLI 手动更新。").await;return Ok(());}
 runtime.send(chat,if arg=="check"{"⏳ 正在检查 Release 更新..."}else{"⏳ 正在检查部署资格并更新 Release，完成后向 Web 服务请求重启..."}).await;
 let answer=match ports.update.as_ref(){Some(port)=>match port.run(arg=="check").await{Ok(result)if result.code==2=>"✅ 已是最新 Release，无需更新。".into(),Ok(result)=>format!("{} {}",if result.code==0{"✅"}else{"❌"},result.output.lines().rfind(|s|!s.is_empty()).unwrap_or("更新命令未返回结果，请查看设置页。")),Err(error)=>format!("❌ 更新失败：{}",error.message())},None=>"❌ 更新失败：Update service unavailable".into()};
 runtime.send(chat,&answer).await;
 },
 "search"=>{
 const USAGE:&str="用法：/search <关键词> [--all] [--limit N] [--role user|assistant]\n示例：/search docker 部署";
 if arg.is_empty(){runtime.send(chat,USAGE).await;return Ok(());}
 let(Some(search),Some(home))=(ports.search.as_ref(),ports.pilot_home.as_ref())else{runtime.send(chat,"聊天记录搜索当前不可用。").await;return Ok(());};
 let project=runtime.state.lock().expect("feishu mapper").mapper.get_project(chat);
 let(params,all)=parse_search(arg,home,project.as_deref());
 let result=search.search(params).await?;
 runtime.send(chat,&format_search(&result,all||project.is_none())).await;
 },
 _=>{}
 }Ok::<(),ChannelError>(())
 }.await;
    if let Err(error) = result {
        runtime.log("error", &format!("command /{name} failed: {error}"));
        runtime
            .send(chat, &format!("命令执行失败: {}", error.message()))
            .await;
    }
    true
}
pub(crate) fn parse_search(
    arg: &str,
    home: &std::path::Path,
    project: Option<&str>,
) -> (JsValue, bool) {
    let mut options =
        serde_json::json!({"pilotHome":home.to_string_lossy(),"regex":false,"caseSensitive":false});
    let tokens = regex::Regex::new(r"[\x09-\x0d\x20\xa0\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+").unwrap().split(crate::metadata::js_trim(arg)).filter(|s|!s.is_empty()).collect::<Vec<_>>();
    let mut terms = vec![];
    let mut i = 0;
    let mut all = false;
    while i < tokens.len() {
        match tokens[i] {
            "--all" | "-a" => all = true,
            "--regex" | "-E" => options["regex"] = true.into(),
            "--case-sensitive" => options["caseSensitive"] = true.into(),
            "--limit" | "-n" => {
                if let Some(n) = tokens
                    .get(i + 1)
                    .map(|s| crate::js_value::string_to_number(s))
                    .filter(|n| n.is_finite() && *n > 0.0)
                {
                    options["limit"] = n.floor().min(100.0).into();
                    i += 1;
                }
            }
            "--role" | "-r" => {
                if let Some(role) = tokens
                    .get(i + 1)
                    .filter(|s| matches!(**s, "all" | "user" | "assistant"))
                {
                    options["role"] = (*role).into();
                    i += 1;
                }
            }
            "--session" | "-s" => {
                if let Some(id) = tokens.get(i + 1) {
                    options["sessionId"] = (*id).into();
                    i += 1;
                }
            }
            word => terms.push(word),
        }
        i += 1;
    }
    options["query"] = terms.join(" ").into();
    if !all && let Some(project) = project {
        options["projectRoot"] = project.into();
    }
    (js(options), all)
}
pub(crate) fn format_search(result: &JsValue, include_project: bool) -> String {
    let query = text(result.get("query"));
    if crate::metadata::js_trim(&query).is_empty() {
        return "用法：/search <关键词> [--all] [--limit N] [--role user|assistant]\n示例：/search docker 部署".into();
    }
    let count = result
        .get("sessionsScanned")
        .and_then(JsValue::as_number)
        .unwrap_or(0.0) as usize;
    let Some(JsValue::Array(matches)) = result.get("matches") else {
        return "命令执行失败: 无效的搜索结果".into();
    };
    if matches.is_empty() {
        return format!("未找到包含「{query}」的聊天记录。\n已扫描 {count} 个会话。");
    }
    let mut lines = vec![
        format!(
            "🔍 找到 {} 条匹配「{query}」{}",
            matches.len(),
            if result.get("truncated") == Some(&JsValue::Bool(true)) {
                "（结果已截断，可加大 --limit）"
            } else {
                ""
            }
        ),
        String::new(),
    ];
    for (i, item) in matches.iter().enumerate() {
        let id = item
            .get("sessionId")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let id = if id.units().len() > 12 {
            format!("{}…", String::from_utf16_lossy(&id.units()[..8]))
        } else {
            String::from_utf16_lossy(id.units())
        };
        let project = text(item.get("projectKey"));
        let suffix = if include_project && !project.is_empty() {
            format!(
                " · {}",
                project
                    .rsplit(['\\', '/'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&project)
            )
        } else {
            String::new()
        };
        let date = text(item.get("createdAt"));
        let date = if date.is_empty() {
            "未知时间".into()
        } else {
            chrono::DateTime::parse_from_rfc3339(&date)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%-m月%-d日 %H:%M")
                        .to_string()
                })
                .unwrap_or(date)
        };
        lines.push(format!(
            "{}. **{}** (\u{60}{}\u{60}){}\n   {} · {}\n   > {}",
            i + 1,
            text(item.get("sessionTitle")),
            id,
            suffix,
            if text(item.get("role")) == "user" {
                "用户"
            } else {
                "助手"
            },
            date,
            text(item.get("snippet"))
        ));
        lines.push(String::new());
    }
    lines.push("提示：在 Web UI 中打开对应会话后，可用 /search 结果中的片段定位消息。".into());
    lines.join("\n").trim_end().into()
}

#[cfg(test)]
mod shared_parser_tests {
    use super::*;
    fn gold() -> serde_json::Value {
        serde_json::from_str(include_str!("commands-oracle.json")).unwrap()
    }
    #[test]
    fn common_registry_resolution_and_search_params_match_actual_ts() {
        for row in gold()["cases"].as_array().unwrap() {
            let input = row["input"].as_str().unwrap();
            let resolved = resolve_command(input);
            assert_eq!(
                resolved.is_some_and(|(name, _)| !matches!(name, "new" | "plan")),
                row["handled"].as_bool().unwrap(),
                "{input}"
            );
            if let Some(("help", _)) = resolved {
                assert_eq!(HELP, row["replies"][0].as_str().unwrap());
            }
            if let Some(("search", arg)) = resolved {
                if arg.is_empty() {
                    continue;
                }
                let (params, _) = parse_search(arg, std::path::Path::new("/fixture/pilot"), None);
                let actual: serde_json::Value = serde_json::from_str(
                    &pilotdeck_protocol::stringify_json(&params)
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                let expected = &row["calls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find_map(|v| v.get("search"))
                    .unwrap();
                assert_eq!(&actual, *expected, "{input}");
                let result = js(
                    serde_json::json!({"query":actual["query"],"matches":[],"sessionsScanned":2,"truncated":false}),
                );
                assert_eq!(
                    format_search(&result, true),
                    row["replies"][0].as_str().unwrap(),
                    "{input}"
                );
            }
        }
    }
}
