//! System commands run against injected host capabilities and the shared gateway.
use super::{delivery, transport::ActiveTransport};
use crate::channel::protocol::{ChannelStatusPort, ChannelUpdatePort};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct HostCommands {
    pub status: Arc<dyn ChannelStatusPort>,
    pub update: Arc<dyn ChannelUpdatePort>,
}
impl HostCommands {
    pub(crate) fn matches(text: &str) -> bool {
        let Some(command) = text.trim().strip_prefix("/") else {
            return false;
        };
        let name = command
            .split_once(" ")
            .map(|(name, _)| name)
            .unwrap_or(command)
            .to_lowercase();
        matches!(
            name.as_str(),
            "status" | "状态" | "update" | "升级" | "更新" | "help" | "帮助"
        )
    }
    pub(crate) async fn execute(
        &self,
        text: &str,
        transport: &ActiveTransport,
        request_id: &delivery::ReplyTarget,
    ) -> bool {
        let trimmed = text.trim();
        let Some(command) = trimmed.strip_prefix("/") else {
            return false;
        };
        let (name, arg) = command
            .split_once(" ")
            .map(|(n, a)| (n.to_lowercase(), a.trim()))
            .unwrap_or_else(|| (command.to_lowercase(), ""));
        match name.as_str() {
            "help" | "帮助" => {
                let help = "📋 可用命令：\n\n/new — Create a new conversation session\n/plan — 先生成计划并请求确认\n/projects (/项目列表) — List available projects\n/switch-project (/切换项目) — Switch active project for this chat\n/update (/升级, /更新) — Install the latest supported Release and request a Web service restart\n/status (/状态) — Show PilotDeck status and version\n/search (/find, /grep, /搜索) — Search chat history across sessions\n/help (/帮助) — Show available commands";
                delivery::reply(transport, request_id.clone(), help.into()).await;
                true
            }
            "status" | "状态" => {
                let answer = match self.status.read().await {
                    Ok(status) => {
                        let minutes = (status.uptime / 60.0).floor() as u64;
                        let uptime = if minutes >= 60 {
                            format!("{}h {}m", minutes / 60, minutes % 60)
                        } else {
                            format!("{minutes}m")
                        };
                        format!(
                            "📊 PilotDeck Status\n\n分支: {}\n提交: {}\n运行时间: {}\nRuntime: {}\n平台: {}",
                            status.branch.trim(),
                            status.commit.trim(),
                            uptime,
                            status.runtime_version,
                            status.platform
                        )
                    }
                    Err(error) => format!("获取状态失败: {}", error.message()),
                };
                delivery::reply(transport, request_id.clone(), answer.into()).await;
                true
            }
            "update" | "升级" | "更新" => {
                if !arg.is_empty() && arg != "check" {
                    delivery::reply(transport, request_id.clone(), "用法：/update check 检查 Release；/update 更新并请求重启。未运行 Web 服务时，请使用 CLI 手动更新。".into()).await;
                    return true;
                }
                let check = arg == "check";
                let progress = if check {
                    "⏳ 正在检查 Release 更新..."
                } else {
                    "⏳ 正在检查部署资格并更新 Release，完成后向 Web 服务请求重启..."
                };
                delivery::reply(transport, request_id.clone(), progress.into()).await;
                let answer = match self.update.run(check).await {
                    Ok(result) if result.code == 2 => "✅ 已是最新 Release，无需更新。".to_string(),
                    Ok(result) => {
                        let last = result
                            .output
                            .lines()
                            .filter(|line| !line.is_empty())
                            .last()
                            .unwrap_or("更新命令未返回结果，请查看设置页。");
                        format!("{} {last}", if result.code == 0 { "✅" } else { "❌" })
                    }
                    Err(error) => format!("❌ 更新失败：{}", error.message()),
                };
                delivery::reply(transport, request_id.clone(), answer.into()).await;
                true
            }
            _ => false,
        }
    }
}
