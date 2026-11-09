//! Shared channel command dispatch (ChannelCommandRegistry.ts:68-297).
//! The channel owns transport, cancellation and persistence; commands use the same
//! gateway port as ordinary messages and never start an agent turn themselves.
use super::routes::{ChannelCall, RouteError};
use pilotdeck_protocol::{JsString, JsValue};
use serde_json::{Value, json};
#[path = "channel_commands_ordered.rs"]
mod ordered;
pub use ordered::ChannelOrderedEventSink;
use std::{collections::HashMap, path::Path, sync::Arc};
#[cfg(test)]
#[path = "channel_commands_tests.rs"]
mod tests;

pub trait ChannelGateway: Send + Sync {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value>;
    fn turn(&self, params: Value) -> ChannelCall<'_, Vec<Value>>;
    fn stream_turn<'a>(
        &'a self,
        params: Value,
        sink: &'a dyn ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            for event in self.turn(params).await? {
                sink.event(event).await?;
            }
            Ok(())
        })
    }
    /// Exact JSON transport for channels whose payloads contain raw UTF-16.
    /// Native WS overrides this; legacy hosts accept only representable JSON.
    fn stream_turn_ordered<'a>(
        &'a self,
        params: JsValue,
        sink: &'a dyn ChannelOrderedEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let params = ordered::serde_view(&params)?;
            self.stream_turn(params, &ordered::LegacySink(sink)).await
        })
    }
    fn update(&self, _check: bool) -> ChannelCall<'_, (i32, String)> {
        Box::pin(async {
            Err(RouteError::new(
                "UPDATE_UNAVAILABLE",
                "更新命令当前不可用。",
            ))
        })
    }
    fn status(&self) -> ChannelCall<'_, String> {
        Box::pin(async {
            Err(RouteError::new(
                "STATUS_UNAVAILABLE",
                "状态提供者当前不可用。",
            ))
        })
    }
    fn search<'a>(
        &'a self,
        _home: &'a Path,
        _project: Option<&'a str>,
        _args: SearchArgs,
    ) -> ChannelCall<'a, String> {
        Box::pin(async { Ok("聊天记录搜索当前不可用。".into()) })
    }
}
pub trait ChannelEventSink: Send + Sync {
    fn event(&self, event: Value) -> ChannelCall<'_, ()>;
}

/// Public WS client bridge. It authenticates once per operation and closes on
/// both success and failure, including a failed streamed agent turn.
pub struct WsChannelGateway {
    origin: String,
    token: String,
    started: std::time::Instant,
    active: std::sync::Mutex<HashMap<JsString, (uuid::Uuid, crate::cron::client::GatewayClient)>>,
}
impl WsChannelGateway {
    pub fn new(origin: String, token: String) -> Arc<Self> {
        Arc::new(Self {
            origin,
            token,
            started: std::time::Instant::now(),
            active: Default::default(),
        })
    }
}
impl ChannelGateway for WsChannelGateway {
    fn stream_turn_ordered<'a>(
        &'a self,
        params: JsValue,
        sink: &'a dyn ChannelOrderedEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let client = crate::cron::client::GatewayClient::connect(&self.origin, &self.token)
                .await
                .map_err(gateway_error)?;
            let session = params
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default();
            let identity = uuid::Uuid::new_v4();
            self.active
                .lock()
                .unwrap()
                .insert(session.clone(), (identity, client.clone()));
            let _guard = ActiveClientGuard {
                active: &self.active,
                session: session.clone(),
                identity,
                client: client.clone(),
            };
            let result = async {
                let mut stream = client
                    .stream_ordered("submit_turn", &params)
                    .await
                    .map_err(gateway_error)?;
                while let Some(event) = stream.next().await {
                    sink.event_ordered(event.map_err(gateway_error)?).await?;
                }
                Ok(())
            }
            .await;
            client.close().await;
            result
        })
    }
    fn status(&self) -> ChannelCall<'_, String> {
        Box::pin(async move {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let git = |args: &'static [&'static str]| {
                let root = root.clone();
                async move {
                    let output = tokio::process::Command::new("git")
                        .args(args)
                        .current_dir(root)
                        .kill_on_drop(true)
                        .output()
                        .await
                        .map_err(|e| RouteError::new("STATUS_FAILED", e.to_string()))?;
                    if !output.status.success() {
                        return Err(RouteError::new(
                            "STATUS_FAILED",
                            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                        ));
                    }
                    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
                }
            };
            let branch = git(&["branch", "--show-current"]).await?;
            let commit = git(&["log", "--oneline", "-1", "HEAD"]).await?;
            let minutes = self.started.elapsed().as_secs() / 60;
            let uptime = if minutes >= 60 {
                format!("{}h {}m", minutes / 60, minutes % 60)
            } else {
                format!("{minutes}m")
            };
            let platform = match std::env::consts::OS {
                "macos" => "darwin",
                "windows" => "win32",
                other => other,
            };
            // ChannelCommandRegistry.ts reports the Node runtime's
            // `process.version`.  Native gateway builds do not embed a
            // fabricated version, so query the same `node` executable that
            // would host the TypeScript implementation.  Keep the command
            // output contract (`vNN...`) and fail the status request when the
            // runtime cannot be discovered, matching the TS handler's
            // catch-and-reply behaviour for a failed status probe.
            let node_output = tokio::process::Command::new("node")
                .arg("--version")
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|e| RouteError::new("STATUS_FAILED", e.to_string()))?;
            if !node_output.status.success() {
                return Err(RouteError::new(
                    "STATUS_FAILED",
                    String::from_utf8_lossy(&node_output.stderr)
                        .trim()
                        .to_owned(),
                ));
            }
            let node_version = String::from_utf8_lossy(&node_output.stdout)
                .trim()
                .to_owned();
            if node_version.is_empty() {
                return Err(RouteError::new(
                    "STATUS_FAILED",
                    "node --version returned no version",
                ));
            }
            Ok(format!(
                "📊 PilotDeck Status\n\n分支: {branch}\n提交: {commit}\n运行时间: {uptime}\nNode: {node_version}\n平台: {platform}"
            ))
        })
    }
    fn update(&self, check: bool) -> ChannelCall<'_, (i32, String)> {
        Box::pin(async move {
            let executable = std::env::current_exe()
                .map_err(|e| RouteError::new("UPDATE_UNAVAILABLE", e.to_string()))?
                .with_file_name("pilotdeck");
            let output = tokio::process::Command::new(executable)
                .args(["update", if check { "--check" } else { "--restart" }])
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|e| RouteError::new("UPDATE_FAILED", e.to_string()))?;
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            Ok((output.status.code().unwrap_or(1), text))
        })
    }
    fn search<'a>(
        &'a self,
        home: &'a Path,
        project: Option<&'a str>,
        args: SearchArgs,
    ) -> ChannelCall<'a, String> {
        Box::pin(async move {
            if args.query.is_empty() {
                return Ok(SEARCH_USAGE.into());
            }
            let executable = std::env::current_exe()
                .map_err(|e| RouteError::new("SEARCH_UNAVAILABLE", e.to_string()))?
                .with_file_name("pilotdeck");
            let mut command = tokio::process::Command::new(executable);
            command
                .args(["chat", "search", &args.query, "--json", "--pilot-home"])
                .arg(home)
                .kill_on_drop(true);
            match project {
                Some(project) => {
                    command.args(["--project", project]);
                }
                None => {
                    command.arg("--all-projects");
                }
            }
            if let Some(limit) = args.limit {
                command.args(["--limit", &limit.to_string()]);
            }
            if let Some(role) = &args.role {
                command.args(["--role", role]);
            }
            if let Some(session) = &args.session_id {
                command.args(["--session", session]);
            }
            if args.regex {
                command.arg("--regex");
            }
            if args.case_sensitive {
                command.arg("--case-sensitive");
            }
            let output = command
                .output()
                .await
                .map_err(|e| RouteError::new("SEARCH_FAILED", e.to_string()))?;
            if !output.status.success() {
                return Err(RouteError::new(
                    "SEARCH_FAILED",
                    String::from_utf8_lossy(&output.stderr)
                        .trim()
                        .trim_start_matches("Error: ")
                        .to_owned(),
                ));
            }
            let result: Value = serde_json::from_slice(&output.stdout)
                .map_err(|e| RouteError::new("SEARCH_FAILED", e.to_string()))?;
            Ok(format_search_result(&result, project.is_none()))
        })
    }
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move {
            let active = params["sessionKey"].as_str().and_then(|key| {
                self.active
                    .lock()
                    .unwrap()
                    .get(&JsString::from(key))
                    .map(|(_, client)| client.clone())
            });
            if let Some(client) = active {
                return client.request(method, &params).await.map_err(gateway_error);
            }
            let client = crate::cron::client::GatewayClient::connect(&self.origin, &self.token)
                .await
                .map_err(gateway_error)?;
            let result = client.request(method, &params).await.map_err(gateway_error);
            client.close().await;
            result
        })
    }
    fn turn(&self, params: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async move {
            let client = crate::cron::client::GatewayClient::connect(&self.origin, &self.token)
                .await
                .map_err(gateway_error)?;
            let result = async {
                let mut stream = client
                    .stream("submit_turn", &params)
                    .await
                    .map_err(gateway_error)?;
                let mut events = Vec::new();
                while let Some(event) = stream.next().await {
                    events.push(event.map_err(gateway_error)?);
                }
                Ok(events)
            }
            .await;
            client.close().await;
            result
        })
    }
    fn stream_turn<'a>(
        &'a self,
        params: Value,
        sink: &'a dyn ChannelEventSink,
    ) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            let client = crate::cron::client::GatewayClient::connect(&self.origin, &self.token)
                .await
                .map_err(gateway_error)?;
            let session = JsString::from(params["sessionKey"].as_str().unwrap_or(""));
            let identity = uuid::Uuid::new_v4();
            self.active
                .lock()
                .unwrap()
                .insert(session.clone(), (identity, client.clone()));
            let _guard = ActiveClientGuard {
                active: &self.active,
                session: session.clone(),
                identity,
                client: client.clone(),
            };
            let result = async {
                let mut stream = client
                    .stream("submit_turn", &params)
                    .await
                    .map_err(gateway_error)?;
                while let Some(event) = stream.next().await {
                    sink.event(event.map_err(gateway_error)?).await?;
                }
                Ok(())
            }
            .await;
            client.close().await;
            result
        })
    }
}
struct ActiveClientGuard<'a> {
    active:
        &'a std::sync::Mutex<HashMap<JsString, (uuid::Uuid, crate::cron::client::GatewayClient)>>,
    session: JsString,
    identity: uuid::Uuid,
    client: crate::cron::client::GatewayClient,
}
impl Drop for ActiveClientGuard<'_> {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap()
            .retain(|key, (identity, _)| key != &self.session || identity != &self.identity);
        let client = self.client.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                client.close().await;
            });
        }
    }
}
const SEARCH_USAGE: &str =
    "用法：/search <关键词> [--all] [--limit N] [--role user|assistant]\n示例：/search docker 部署";
pub fn format_search_result(result: &Value, include_project: bool) -> String {
    let query = result["query"].as_str().unwrap_or("");
    if crate::metadata::js_trim(query).is_empty() {
        return SEARCH_USAGE.into();
    }
    let matches = result["matches"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if matches.is_empty() {
        return format!(
            "未找到包含「{query}」的聊天记录。\n已扫描 {} 个会话。",
            result["sessionsScanned"].as_u64().unwrap_or(0)
        );
    }
    let mut lines = vec![
        format!(
            "🔍 找到 {} 条匹配「{query}」{}",
            matches.len(),
            if result["truncated"] == true {
                "（结果已截断，可加大 --limit）"
            } else {
                ""
            }
        ),
        String::new(),
    ];
    for (index, item) in matches.iter().enumerate() {
        let field = |key| item[key].as_str().unwrap_or("");
        let id = field("sessionId");
        let units: Vec<_> = id.encode_utf16().collect();
        let short = if units.len() > 12 {
            format!("{}…", String::from_utf16_lossy(&units[..8]))
        } else {
            id.into()
        };
        let project = if include_project && !field("projectKey").is_empty() {
            format!(
                " · {}",
                field("projectKey")
                    .rsplit(['/', '\\'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(field("projectKey"))
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "{}. **{}** (`{short}`){project}\n   {} · {}\n   > {}",
            index + 1,
            field("sessionTitle"),
            if field("role") == "user" {
                "用户"
            } else {
                "助手"
            },
            format_when(field("createdAt")),
            field("snippet")
        ));
        lines.push(String::new());
    }
    lines.push("提示：在 Web UI 中打开对应会话后，可用 /search 结果中的片段定位消息。".into());
    lines.join("\n").trim_end().into()
}
fn format_when(value: &str) -> String {
    if value.is_empty() {
        return "未知时间".into();
    }
    let Ok(Some(ms)) = pilotdeck_transport::system_clock::parse_node22_date(&value.into()) else {
        return value.into();
    };
    let local = (|| {
        let path =
            pilotdeck_transport::system_clock::system_tzif_path(std::env::var_os("TZ").as_deref())?;
        let bytes = std::fs::read(path).ok()?;
        let zone = tz::TimeZone::from_tz_data(&bytes).ok()?;
        tz::DateTime::from_timespec((ms / 1000.0).floor() as i64, 0, zone.as_ref()).ok()
    })();
    match local {
        Some(local) => format!(
            "{}月{}日 {:02}:{:02}",
            local.month(),
            local.month_day(),
            local.hour(),
            local.minute()
        ),
        None => value.into(),
    }
}
fn gateway_error(error: crate::cron::client::GatewayClientError) -> RouteError {
    RouteError::new(error.code, error.message)
}
pub trait ChannelReply: Send + Sync {
    fn reply<'a>(&'a self, text: &'a str) -> ChannelCall<'a, ()>;
}

#[derive(Clone, Default)]
pub struct ChannelSessionState {
    pub active_by_chat_id: HashMap<String, String>,
    pub project_by_chat_id: HashMap<String, String>,
}
impl ChannelSessionState {
    pub fn from_value(value: &Value) -> Self {
        let map = |name| {
            value
                .get(name)
                .and_then(Value::as_object)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            active_by_chat_id: map("activeByChatId"),
            project_by_chat_id: map("projectByChatId"),
        }
    }
    pub fn snapshot(&self) -> Value {
        json!({"activeByChatId":self.active_by_chat_id,"projectByChatId":self.project_by_chat_id})
    }
    pub fn session(&self, channel: &str, chat: &str) -> String {
        self.active_by_chat_id
            .get(chat)
            .cloned()
            .unwrap_or_else(|| format!("{channel}:chat={chat}:general"))
    }
    pub fn reset(&mut self, channel: &str, chat: &str) -> String {
        let session = format!("{channel}:chat={chat}:s_{}", uuid::Uuid::new_v4());
        self.active_by_chat_id.insert(chat.into(), session.clone());
        session
    }
    pub fn persist_channel_mapper(
        state: &ChannelSessionState,
        on_state_change: &Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) {
        if let Some(save) = on_state_change {
            save(state.snapshot());
        }
    }
    pub fn project(&self, chat: &str) -> Option<&str> {
        self.project_by_chat_id.get(chat).map(String::as_str)
    }
}

const COMMANDS: &[(&str, &[&str], &str, bool)] = &[
    ("new", &[], "Create a new conversation session", true),
    ("plan", &[], "先生成计划并请求确认", false),
    ("projects", &["项目列表"], "List available projects", true),
    (
        "switch-project",
        &["切换项目"],
        "Switch active project for this chat",
        true,
    ),
    (
        "update",
        &["升级", "更新"],
        "Install the latest supported Release and request a Web service restart",
        true,
    ),
    (
        "status",
        &["状态"],
        "Show PilotDeck status and version",
        true,
    ),
    (
        "search",
        &["find", "grep", "搜索"],
        "Search chat history across sessions",
        true,
    ),
    ("help", &["帮助"], "Show available commands", true),
];
pub fn resolve_command(text: &str) -> Option<(&'static str, &str)> {
    let text = crate::metadata::js_trim(text).strip_prefix('/')?;
    let (name, arg) = text.split_once(' ').unwrap_or((text, ""));
    let name = name.to_lowercase();
    COMMANDS
        .iter()
        .find(|(n, aliases, _, _)| *n == name || aliases.contains(&name.as_str()))
        .map(|(n, _, _, _)| (*n, crate::metadata::js_trim(arg)))
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchArgs {
    pub query: String,
    pub all_projects: bool,
    pub limit: Option<usize>,
    pub regex: bool,
    pub case_sensitive: bool,
    pub role: Option<String>,
    pub session_id: Option<String>,
}
/// Preserve the TS parser's invalid-option behavior: an invalid flag operand is
/// reconsidered as a query token on the next iteration.
pub fn parse_search_args(raw: &str) -> SearchArgs {
    let tokens: Vec<_> = raw.split_whitespace().collect();
    let mut out = SearchArgs::default();
    let mut query = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let next = tokens.get(at + 1).copied();
        match tokens[at] {
            "--all" | "-a" => out.all_projects = true,
            "--regex" | "-E" => out.regex = true,
            "--case-sensitive" => out.case_sensitive = true,
            "--limit" | "-n" => {
                if let Some(value) = next
                    .and_then(js_number)
                    .filter(|n| n.is_finite() && *n > 0.0)
                {
                    out.limit = Some(value.floor().min(100.0) as usize);
                    at += 1;
                }
            }
            "--role" | "-r" => {
                if let Some(value @ ("user" | "assistant" | "all")) = next {
                    out.role = Some(value.into());
                    at += 1;
                }
            }
            "--session" | "-s" => {
                if let Some(value) = next {
                    out.session_id = Some(value.into());
                    at += 1;
                }
            }
            token => query.push(token),
        }
        at += 1;
    }
    out.query = query.join(" ");
    out
}
fn js_number(raw: &str) -> Option<f64> {
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(value) = raw.strip_prefix(prefix) {
            return u64::from_str_radix(value, radix).ok().map(|n| n as f64);
        }
    }
    raw.parse().ok()
}

pub struct CommandContext<'a> {
    pub gateway: &'a dyn ChannelGateway,
    pub reply: &'a dyn ChannelReply,
    pub state: &'a mut ChannelSessionState,
    pub channel: &'a str,
    pub chat: &'a str,
    pub home: &'a Path,
}
pub async fn execute_channel_command(
    text: &str,
    ctx: &mut CommandContext<'_>,
) -> super::routes::Result<bool> {
    let Some((name, arg)) = resolve_command(text) else {
        return Ok(false);
    };
    if matches!(name, "new" | "plan") {
        return Ok(false);
    }
    let result = execute(name, arg, ctx).await;
    if let Err(error) = result {
        ctx.reply
            .reply(&format!("命令执行失败: {}", error.message))
            .await?;
    }
    Ok(true)
}
async fn execute(name: &str, arg: &str, ctx: &mut CommandContext<'_>) -> super::routes::Result<()> {
    match name {
        "projects" | "switch-project" => {
            if name == "switch-project" && arg.is_empty() {
                return ctx
                    .reply
                    .reply("用法：/switch-project <项目名>\n\n发送 /projects 查看可用项目。")
                    .await;
            }
            let result = ctx.gateway.request("list_projects", json!({})).await?;
            let projects = result
                .get("projects")
                .and_then(Value::as_array)
                .ok_or_else(|| RouteError::new("INVALID_PROJECTS", "Invalid projects response"))?;
            let field =
                |p: &Value, k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
            if name == "projects" {
                if projects.is_empty() {
                    return ctx
                        .reply
                        .reply("暂无项目。使用 Web UI 创建 WorkSpace 后即可在此切换。")
                        .await;
                }
                let mut lines = vec!["📂 项目列表：".into(), "".into()];
                for p in projects {
                    let marker = if ctx.state.project(ctx.chat)
                        == p.get("projectKey").and_then(Value::as_str)
                    {
                        " ✅"
                    } else {
                        ""
                    };
                    lines.push(format!("• {}{marker}", field(p, "name")));
                }
                lines.extend([
                    "".into(),
                    "发送 /switch-project <项目名> 切换 WorkSpace".into(),
                ]);
                ctx.reply.reply(&lines.join("\n")).await
            } else {
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
                let Some(target) = target else {
                    return ctx
                        .reply
                        .reply(&format!(
                            "未找到匹配「{arg}」的项目。\n\n发送 /projects 查看可用项目。"
                        ))
                        .await;
                };
                ctx.state
                    .project_by_chat_id
                    .insert(ctx.chat.into(), field(target, "projectKey"));
                ctx.state.reset(ctx.channel, ctx.chat);
                ctx.reply
                    .reply(&format!(
                        "已切换到项目：{}\n路径：{}\n（已自动创建新会话）",
                        field(target, "name"),
                        field(target, "fullPath")
                    ))
                    .await
            }
        }
        "help" => {
            let mut lines = vec!["📋 可用命令：".into(), "".into()];
            for (name, aliases, description, _) in COMMANDS {
                let aliases = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(
                        " ({})",
                        aliases
                            .iter()
                            .map(|a| format!("/{a}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                lines.push(format!("/{name}{aliases} — {description}"));
            }
            ctx.reply.reply(&lines.join("\n")).await
        }
        "search" => {
            if arg.is_empty() {
                return ctx.reply.reply("用法：/search <关键词> [--all] [--limit N] [--role user|assistant]\n示例：/search docker 部署").await;
            }
            let args = parse_search_args(arg);
            let project = if args.all_projects {
                None
            } else {
                ctx.state.project(ctx.chat)
            };
            let text = ctx.gateway.search(ctx.home, project, args).await?;
            ctx.reply.reply(&text).await
        }
        "status" => match ctx.gateway.status().await {
            Ok(text) => ctx.reply.reply(&text).await,
            Err(e) => {
                ctx.reply
                    .reply(&format!("获取状态失败: {}", e.message))
                    .await
            }
        },
        "update" => {
            if !arg.is_empty() && arg != "check" {
                return ctx.reply.reply("用法：/update check 检查 Release；/update 更新并请求重启。未运行 Web 服务时，请使用 CLI 手动更新。").await;
            }
            ctx.reply
                .reply(if arg == "check" {
                    "⏳ 正在检查 Release 更新..."
                } else {
                    "⏳ 正在检查部署资格并更新 Release，完成后向 Web 服务请求重启..."
                })
                .await?;
            let text = match ctx.gateway.update(arg == "check").await {
                Ok((2, _)) => "✅ 已是最新 Release，无需更新。".into(),
                Ok((code, output)) => format!(
                    "{} {}",
                    if code == 0 { "✅" } else { "❌" },
                    output
                        .lines()
                        .filter(|line| !line.is_empty())
                        .next_back()
                        .unwrap_or("更新命令未返回结果，请查看设置页。")
                ),
                Err(e) => format!("❌ 更新失败：{}", e.message),
            };
            ctx.reply.reply(&text).await
        }
        _ => Ok(()),
    }
}
