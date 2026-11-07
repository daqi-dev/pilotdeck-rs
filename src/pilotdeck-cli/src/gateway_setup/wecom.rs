use super::{
    config::{self, SetupPaths},
    http::Fetch,
    prompt::{Prompt, SetupResult},
};
use pilotdeck_protocol::{JsString, JsValue};

const WS: &str = "wss://openws.work.weixin.qq.com";
const GENERATE: &str = "https://work.weixin.qq.com/ai/qc/generate?source=hermes";
const QUERY: &str = "https://work.weixin.qq.com/ai/qc/query_result?scode=";
const PAGE: &str = "https://work.weixin.qq.com/ai/qc/gen?source=hermes&scode=";
const TIMEOUT_MS: u64 = 300_000;
const POLL_MS: u64 = 3000;

fn nullish(values: &[&JsValue]) -> String {
    values
        .iter()
        .find(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
        .map_or_else(String::new, |value| config::text(value))
}
fn list(value: &JsValue) -> Vec<JsValue> {
    let values = match value {
        JsValue::Array(values) => values.iter().map(config::text).collect(),
        JsValue::String(value) => String::from_utf16_lossy(value.units())
            .split(',')
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    values
        .into_iter()
        .map(|value| crate::paths::js_trim(&value).to_owned())
        .filter(|value| !value.is_empty())
        .map(config::string)
        .collect()
}
fn policy(value: &JsValue, fallback: &str) -> String {
    let value = crate::paths::js_trim(&config::text(value)).to_lowercase();
    if matches!(value.as_str(), "open" | "allowlist" | "disabled") {
        value
    } else {
        fallback.into()
    }
}
struct Settings {
    bot: String,
    secret: String,
    websocket: String,
    dm: String,
    allow: Vec<JsValue>,
    group: String,
    group_allow: Vec<JsValue>,
}
fn save(
    paths: &SetupPaths,
    settings: Settings,
    groups: Option<pilotdeck_yaml::value::ValueGraph>,
) -> SetupResult<()> {
    let mut extra = config::object([
        ("secret", config::string(settings.secret)),
        (
            "websocket_url",
            config::string(if settings.websocket.is_empty() {
                WS.into()
            } else {
                settings.websocket
            }),
        ),
        ("dm_policy", config::string(settings.dm)),
        ("group_policy", config::string(settings.group)),
    ]);
    let JsValue::Object(object) = &mut extra else {
        unreachable!()
    };
    if !settings.allow.is_empty() {
        object.insert("allow_from".into(), JsValue::Array(settings.allow));
    }
    if !settings.group_allow.is_empty() {
        object.insert(
            "group_allow_from".into(),
            JsValue::Array(settings.group_allow),
        );
    }
    config::write_wecom_adapter(
        &paths.config,
        config::object([
            ("enabled", JsValue::Bool(true)),
            ("token", config::string(settings.bot)),
            ("extra", extra),
        ]),
        groups,
    )
}

pub(super) fn setup(
    paths: &SetupPaths,
    io: &mut dyn Prompt,
    network: &mut dyn Fetch,
) -> SetupResult<()> {
    io.line("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")?;
    io.line("  企业微信 WeCom AI Bot 配置")?;
    io.line("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n")?;
    let (root, groups) = config::wecom_defaults(&paths.config);
    let current = config::get(&root, &["adapters", "wecom"]);
    let bot = nullish(&[
        config::get(current, &["token"]),
        config::get(current, &["extra", "bot_id"]),
        config::get(current, &["extra", "botId"]),
        &config::string(""),
    ]);
    let secret = nullish(&[
        config::get(current, &["extra", "secret"]),
        config::get(current, &["extra", "botSecret"]),
        &config::string(""),
    ]);
    let websocket = nullish(&[
        config::get(current, &["extra", "websocket_url"]),
        config::get(current, &["extra", "websocketUrl"]),
        &config::string(WS),
    ]);
    if !bot.is_empty() && !secret.is_empty() {
        io.line(&format!(
            "已有企业微信配置 (Bot ID: {})",
            super::feishu::mask(&bot)
        ))?;
        if !matches!(io.ask("重新配置？ [y/N]: ")?.as_str(), "y" | "yes") {
            let extra = config::get(current, &["extra"]);
            save(
                paths,
                Settings {
                    bot,
                    secret,
                    websocket,
                    dm: policy(config::get(extra, &["dm_policy"]), "open"),
                    allow: list(first(
                        config::get(extra, &["allow_from"]),
                        config::get(extra, &["allowFrom"]),
                    )),
                    group: policy(config::get(extra, &["group_policy"]), "disabled"),
                    group_allow: list(first(
                        config::get(extra, &["group_allow_from"]),
                        config::get(extra, &["groupAllowFrom"]),
                    )),
                },
                groups,
            )?;
            io.line("企业微信已启用（使用现有凭据）。\n")?;
            return Ok(());
        }
    }
    io.line("企业微信 AI Bot 通过官方 WebSocket 网关连接，无需公网回调地址。")?;
    io.line("请确保你有企业微信组织管理员权限，或已在管理后台拿到 Bot ID 和 Secret。\n")?;
    let scan = io.ask("是否扫码自动创建/获取 Bot ID 和 Secret？ [Y/n]: ")?;
    let credentials = if matches!(scan.as_str(), "n" | "no") {
        None
    } else {
        qr_login(io, network)?
    };
    if credentials.is_none() && !matches!(scan.as_str(), "n" | "no") {
        io.line("\n扫码未完成或接口不可用，将使用手动输入模式。\n")?;
    }
    let (mut bot_id, mut new_secret) = credentials.unwrap_or_default();
    if bot_id.is_empty() || new_secret.is_empty() {
        io.line("手动配置步骤：")?;
        io.line("  1. 登录企业微信管理后台")?;
        io.line("  2. 进入应用管理，创建 AI Bot / 智能机器人")?;
        io.line("  3. 选择 API 模式")?;
        io.line("  4. 复制 Bot ID 和 Secret\n")?;
        let hint = if bot.is_empty() {
            String::new()
        } else {
            format!(" (当前: {})", super::feishu::mask(&bot))
        };
        let answer = io.text(&format!("Bot ID{hint}: "))?;
        bot_id = if answer.is_empty() { bot } else { answer };
        let hint = if secret.is_empty() {
            ""
        } else {
            " (回车保留当前值)"
        };
        let answer = io.text(&format!("Secret{hint}: "))?;
        new_secret = if answer.is_empty() { secret } else { answer };
    }
    if bot_id.is_empty() || new_secret.is_empty() {
        io.line("\n未提供完整 Bot ID / Secret，跳过企业微信配置。\n")?;
        return Ok(());
    }
    let answer = io.text(&format!("WebSocket URL (默认: {WS}): "))?;
    let websocket = if answer.is_empty() { WS.into() } else { answer };
    let choice = io.text("私聊访问策略 [1=open 推荐, 2=allowlist, 3=disabled] (默认 1): ")?;
    let mut dm = match choice.as_str() {
        "2" => "allowlist",
        "3" => "disabled",
        _ => "open",
    };
    let allow = if dm == "allowlist" {
        list(&config::string(io.text(
            "允许私聊的企业微信用户 ID（逗号分隔，留空则暂时禁用私聊）: ",
        )?))
    } else {
        Vec::new()
    };
    if dm == "allowlist" && allow.is_empty() {
        dm = "disabled";
    }
    let choice = io.text("群聊访问策略 [1=disabled 推荐, 2=open, 3=allowlist] (默认 1): ")?;
    let group = match choice.as_str() {
        "2" => "open",
        "3" => "allowlist",
        _ => "disabled",
    };
    let group_allow = if group == "allowlist" {
        list(&config::string(
            io.text("允许响应的群聊 chat_id（逗号分隔）: ")?,
        ))
    } else {
        Vec::new()
    };
    save(
        paths,
        Settings {
            bot: bot_id,
            secret: new_secret,
            websocket,
            dm: dm.into(),
            allow,
            group: group.into(),
            group_allow,
        },
        None,
    )?;
    io.line("\n企业微信配置已写入 pilotdeck.yaml")?;
    io.line("连接模式: AI Bot WebSocket (无需公网 IP)")?;
    io.line("重启 PilotDeck 服务后生效。\n")
}
fn first<'a>(a: &'a JsValue, b: &'a JsValue) -> &'a JsValue {
    if matches!(a, JsValue::Null | JsValue::Undefined) {
        b
    } else {
        a
    }
}
fn js_string(value: &JsValue) -> JsString {
    match value {
        JsValue::String(value) => value.clone(),
        JsValue::Array(values) => {
            let mut units = Vec::new();
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    units.push(44);
                }
                if !matches!(value, JsValue::Null | JsValue::Undefined) {
                    units.extend_from_slice(js_string(value).units());
                }
            }
            JsString::from_utf16(units)
        }
        _ => config::text(value).into(),
    }
}
fn trim_units(value: JsString) -> JsString {
    let space = |unit: &u16| {
        char::from_u32(u32::from(*unit)).is_some_and(|character| {
            let mut bytes = [0; 4];
            crate::paths::js_trim(character.encode_utf8(&mut bytes)).is_empty()
        })
    };
    let units = value.units();
    let start = units
        .iter()
        .position(|unit| !space(unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !space(unit))
        .map_or(start, |index| index + 1);
    JsString::from_utf16(units[start..end].to_vec())
}
fn component(value: &JsString) -> Result<String, String> {
    let value = value.to_utf8().map_err(|_| "URI malformed".to_owned())?;
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    Ok(out)
}

fn qr_login(io: &mut dyn Prompt, network: &mut dyn Fetch) -> SetupResult<Option<(String, String)>> {
    match qr_attempt(io, network) {
        Ok(result) => Ok(result),
        Err(error) => {
            io.line(&format!("企业微信扫码失败: {error}"))?;
            Ok(None)
        }
    }
}
fn qr_attempt(
    io: &mut dyn Prompt,
    network: &mut dyn Fetch,
) -> Result<Option<(String, String)>, String> {
    io.line("正在连接企业微信生成扫码链接...")
        .map_err(|error| format!("{error:?}"))?;
    let generated = network.success_json(GENERATE)?;
    let data = config::get(&generated, &["data"]);
    let raw_scode = config::get(data, &["scode"]);
    let scode = trim_units(if matches!(raw_scode, JsValue::Null | JsValue::Undefined) {
        JsString::from("")
    } else {
        js_string(raw_scode)
    });
    let auth_url = crate::paths::js_trim(&nullish(&[
        config::get(data, &["auth_url"]),
        &config::string(""),
    ]))
    .to_owned();
    if scode.is_empty() || auth_url.is_empty() {
        io.line("企业微信扫码接口返回格式异常。")
            .map_err(|error| format!("{error:?}"))?;
        return Ok(None);
    }
    let encoded = component(&scode)?;
    io.line("\n请用企业微信手机端扫描/打开以下链接：")
        .map_err(|error| format!("{error:?}"))?;
    io.line(&auth_url).map_err(|error| format!("{error:?}"))?;
    io.line(&format!(
        "\n如果无法直接扫码，请打开：\n{PAGE}{}\n",
        encoded
    ))
    .map_err(|error| format!("{error:?}"))?;
    io.line("等待扫码结果...")
        .map_err(|error| format!("{error:?}"))?;
    let deadline = network.now_ms().saturating_add(TIMEOUT_MS);
    let url = format!("{QUERY}{encoded}");
    while network.now_ms() < deadline {
        let response = network.success_json(&url).unwrap_or(JsValue::Undefined);
        let data = config::get(&response, &["data"]);
        let status = nullish(&[config::get(data, &["status"]), &config::string("")]).to_lowercase();
        if status == "success" {
            let bot = config::get(data, &["bot_info"]);
            let bot_id = crate::paths::js_trim(&nullish(&[
                config::get(bot, &["botid"]),
                config::get(bot, &["bot_id"]),
                &config::string(""),
            ]))
            .to_owned();
            let secret = crate::paths::js_trim(&nullish(&[
                config::get(bot, &["secret"]),
                &config::string(""),
            ]))
            .to_owned();
            if !bot_id.is_empty() && !secret.is_empty() {
                return Ok(Some((bot_id, secret)));
            }
            io.line("扫码成功但未返回完整 Bot 凭据。")
                .map_err(|error| format!("{error:?}"))?;
            return Ok(None);
        }
        network.sleep(POLL_MS);
    }
    io.line("企业微信扫码超时。")
        .map_err(|error| format!("{error:?}"))?;
    Ok(None)
}
