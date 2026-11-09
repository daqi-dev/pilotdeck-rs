use super::{
    config::{self, SetupPaths},
    http::Fetch,
    prompt::{Prompt, SetupResult},
};
use pilotdeck_protocol::JsValue;

pub(super) fn mask(value: &str) -> String {
    let units: Vec<_> = value.encode_utf16().collect();
    if units.len() <= 8 {
        value.into()
    } else {
        format!(
            "{}…{}",
            String::from_utf16_lossy(&units[..4]),
            String::from_utf16_lossy(&units[units.len() - 4..])
        )
    }
}
pub(super) fn setup(
    paths: &SetupPaths,
    io: &mut dyn Prompt,
    network: &mut dyn Fetch,
) -> SetupResult<()> {
    io.line("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")?;
    io.line("  飞书 / Lark 配置")?;
    io.line("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n")?;
    let config = config::load(&paths.config);
    let app_id = config::fallback(config::get(&config, &["adapters", "feishu", "appId"]), "");
    let secret = config::fallback(
        config::get(&config, &["adapters", "feishu", "appSecret"]),
        "",
    );
    let old_domain = config::fallback(
        config::get(&config, &["adapters", "feishu", "domainName"]),
        "feishu",
    );
    let choice = io.ask(&format!(
        "使用哪个平台？ [feishu/lark] (当前: {old_domain}): "
    ))?;
    let domain = if choice == "lark" { "lark" } else { "feishu" };
    io.settle();
    let host = if domain == "lark" {
        "https://open.larksuite.com"
    } else {
        "https://open.feishu.cn"
    };
    // SDK 1.65.0 does not export AppTicketManager (runtime probe retained in
    // issue-124 evidence), so the baseline's optional QR branch is unreachable.
    io.line(&format!("\n请在 {host} 创建企业自建应用："))?;
    io.line("  1. 创建应用 → 获取 App ID 和 App Secret")?;
    io.line("  2. 权限管理 → 添加以下权限：")?;
    io.line("     - im:message (收发消息)")?;
    io.line("     - im:message:send_as_bot (以机器人身份发消息)")?;
    io.line("     - im:resource (访问图片/文件)")?;
    io.line("     - im:chat (群聊元信息)")?;
    io.line("  3. 事件订阅 → 添加 im.message.receive_v1")?;
    io.line("  4. 版本管理 → 发布版本\n")?;
    let hint = if app_id.is_empty() {
        String::new()
    } else {
        format!(" (当前: {})", mask(&app_id))
    };
    let answer = io.text(&format!("App ID{hint}: "))?;
    let app_id = if answer.is_empty() { app_id } else { answer };
    let hint = if secret.is_empty() {
        ""
    } else {
        " (回车保留当前值)"
    };
    let answer = io.text(&format!("App Secret{hint}: "))?;
    let secret = if answer.is_empty() { secret } else { answer };
    if app_id.is_empty() || secret.is_empty() {
        io.line("\n⚠️  未提供完整凭据，跳过飞书配置。")?;
        return Ok(());
    }
    io.line("\n🔍 验证凭据...")?;
    let body = serde_json::json!({"app_id":app_id,"app_secret":secret}).to_string();
    let validation = network.json(
        "POST",
        &format!("{host}/open-apis/auth/v3/tenant_access_token/internal"),
        Some(&body),
    );
    let error = match validation {
        Ok(response) => {
            // fetch().json() in the baseline validates the JSON fields even
            // when the response has a non-success HTTP status.
            let _http_status = response.status;
            let value = &response.json;
            if matches!(config::get(value,&["code"]),JsValue::Number(n) if *n==0.0)
                && config::truthy(config::get(value, &["tenant_access_token"]))
            {
                None
            } else {
                Some(format!(
                    "code={} msg={}",
                    config::text(config::get(value, &["code"])),
                    if matches!(
                        config::get(value, &["msg"]),
                        JsValue::Null | JsValue::Undefined
                    ) {
                        "unknown".into()
                    } else {
                        config::text(config::get(value, &["msg"]))
                    }
                ))
            }
        }
        Err(error) => Some(error),
    };
    if let Some(error) = error {
        io.line(&format!("\n❌ 凭据验证失败: {error}"))?;
        if !matches!(io.ask("是否仍然保存配置？ [y/N]: ")?.as_str(), "y" | "yes") {
            io.line("已取消。")?;
            return Ok(());
        }
    } else {
        io.line("✅ 凭据验证通过！")?;
    }
    config::write_adapter(
        &paths.config,
        "feishu",
        config::object([
            ("enabled", JsValue::Bool(true)),
            ("appId", config::string(app_id)),
            ("appSecret", config::string(secret)),
            ("connectionMode", config::string("stream")),
            ("domainName", config::string(domain)),
        ]),
    )?;
    io.line("\n✅ 飞书配置已写入 pilotdeck.yaml")?;
    io.line("   连接模式: stream (WebSocket, 推荐 — 无需公网 IP)")?;
    io.line("   重启 PilotDeck 服务后生效\n")
}
