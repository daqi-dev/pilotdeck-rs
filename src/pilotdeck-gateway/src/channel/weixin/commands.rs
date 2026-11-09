//! Weixin transport/mapper adaptation of the common fixed TS registry.
use super::*;
use crate::channel::feishu::commands::{HELP, format_search, parse_search, resolve_command};
use crate::channel::protocol::ChannelError;
fn text(value: Option<&JsValue>) -> String {
    value
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_default()
}
fn js(value: serde_json::Value) -> JsValue {
    pilotdeck_protocol::parse_json(&value.to_string()).unwrap()
}
fn failure(message: &str) -> ChannelError {
    ChannelError::error(message)
}
fn save(channel: &WeixinChannel) {
    if let Some(save) = &channel.0.on_state_change {
        save(&channel.0.mapper.snapshot());
    }
}
async fn reply(channel: &WeixinChannel, user: &JsString, message: &str, _queue: bool) {
    channel
        .send_reply(user, &JsString::from(message), false)
        .await;
}
pub(super) fn is_system_command(input: &str) -> bool {
    resolve_command(input).is_some_and(|(name, _)| !matches!(name, "new" | "plan"))
}
pub(super) async fn execute(channel: &WeixinChannel, user: &JsString, input: &str) -> bool {
    let Some((name, arg)) = resolve_command(input) else {
        return false;
    };
    if matches!(name, "new" | "plan") {
        return false;
    }
    let ports = channel.0.host_ports.clone();
    let result=async{
 match name{
 "help"=>{reply(channel,user,HELP, false).await;},
 "projects"|"switch-project"=>{
 if name=="switch-project"&&arg.is_empty(){reply(channel,user,"用法：/switch-project <项目名>\n\n发送 /projects 查看可用项目。", false).await;return Ok(());}
 let control=ports.control.as_ref().ok_or_else(||failure("Gateway not ready"))?;
 let value=control.call("list_projects",js(serde_json::json!({}))).await?;
 let projects=value.get("projects").and_then(|v|if let JsValue::Array(a)=v{Some(a)}else{None}).ok_or_else(||failure("Invalid project list"))?;
 if name=="projects"{
 if projects.is_empty(){reply(channel,user,"暂无项目。使用 Web UI 创建 WorkSpace 后即可在此切换。", false).await;return Ok(());}
 let current=channel.0.mapper.get_project(user).map(|s|String::from_utf16_lossy(s.units()));
 let mut lines=vec!["📂 项目列表：".to_owned(),String::new()];
 for project in projects{lines.push(format!("• {}{}",text(project.get("name")),if current.as_deref()==Some(text(project.get("projectKey")).as_str()){" ✅"}else{""}));}
 lines.push(String::new());lines.push("发送 /switch-project <项目名> 切换 WorkSpace".into());reply(channel,user,&lines.join("\n"), false).await;
 }else{
 let lower=arg.to_lowercase();let target=projects.iter().find(|p|text(p.get("name"))==arg).or_else(||projects.iter().find(|p|text(p.get("name")).to_lowercase()==lower)).or_else(||projects.iter().find(|p|text(p.get("name")).to_lowercase().contains(&lower)));
 let Some(target)=target else{reply(channel,user,&format!("未找到匹配「{arg}」的项目。\n\n发送 /projects 查看可用项目。"), false).await;return Ok(());};
 channel.0.mapper.bind_project(user,&JsString::from(text(target.get("projectKey"))));save(channel);channel.0.mapper.resolve(user,&JsString::from("/new"));save(channel);
 reply(channel,user,&format!("已切换到项目：{}\n路径：{}\n（已自动创建新会话）",text(target.get("name")),text(target.get("fullPath"))), false).await;
 }
 },
 "status"=>{
 let status=ports.status.as_ref().ok_or_else(||failure("Status service unavailable"))?.read().await?;
 let min=(status.uptime/60.0).floor()as u64;let uptime=if min>=60{format!("{}h {}m",min/60,min%60)}else{format!("{min}m")};
 reply(channel,user,&format!("📊 PilotDeck Status\n\n分支: {}\n提交: {}\n运行时间: {uptime}\nRuntime: {}\n平台: {}",status.branch.trim(),status.commit.trim(),status.runtime_version,status.platform), false).await;
 },
 "update"=>{
 if !arg.is_empty()&&arg!="check"{reply(channel,user,"用法：/update check 检查 Release；/update 更新并请求重启。未运行 Web 服务时，请使用 CLI 手动更新。", false).await;return Ok(());}
 reply(channel,user,if arg=="check"{"⏳ 正在检查 Release 更新..."}else{"⏳ 正在检查部署资格并更新 Release，完成后向 Web 服务请求重启..."}, false).await;
 let answer=match ports.update.as_ref(){Some(port)=>match port.run(arg=="check").await{Ok(result)if result.code==2=>"✅ 已是最新 Release，无需更新。".into(),Ok(result)=>format!("{} {}",if result.code==0{"✅"}else{"❌"},result.output.lines().filter(|s|!s.is_empty()).last().unwrap_or("更新命令未返回结果，请查看设置页。")),Err(error)=>format!("❌ 更新失败：{}",error.message())},None=>"❌ 更新失败：Update service unavailable".into()};
 reply(channel,user,&answer, false).await;
 },
 "search"=>{
 const USAGE:&str="用法：/search <关键词> [--all] [--limit N] [--role user|assistant]\n示例：/search docker 部署";
 if arg.is_empty(){reply(channel,user,USAGE, false).await;return Ok(());}
 let(Some(search),Some(home))=(ports.search.as_ref(),ports.pilot_home.as_ref())else{reply(channel,user,"聊天记录搜索当前不可用。", false).await;return Ok(());};
 let project=channel.0.mapper.get_project(user).map(|s|String::from_utf16_lossy(s.units()));
 let(params,all)=parse_search(arg,home,project.as_deref());
 let result=search.search(params).await?;
 reply(channel,user,&format_search(&result,all||project.is_none()), false).await;
 },
 _=>{}
 }Ok::<(),ChannelError>(())
 }.await;
    if let Err(error) = result {
        channel.log("error", format!("command /{name} failed: {error}"));
        reply(
            channel,
            user,
            &format!("命令执行失败: {}", error.message()),
            false,
        )
        .await;
    }
    true
}
