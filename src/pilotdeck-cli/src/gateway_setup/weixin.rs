//! Setup's external weixin-ilink@0.1.0 adapter. Transport, JS codec and
//! transactional file publication are shared with the other native setup paths.
use super::{
    config::{self, SetupPaths},
    http::{Fetch, IlinkFailure},
    prompt::{Prompt, SetupResult},
    save,
};
use crate::{Failure, paths};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::{fs, io::Read, os::unix::fs::OpenOptionsExt};

pub(super) const CREDENTIAL_LIMIT: u64 = 1024 * 1024;

const POLL_MS: u64 = 1000;
const LOGIN_MS: u64 = 480_000;
struct Credentials {
    base: JsValue,
    token: JsValue,
    account: JsValue,
}
fn load(paths: &SetupPaths) -> SetupResult<Option<JsValue>> {
    // O_NONBLOCK prevents a hostile FIFO from blocking before fstat. The opened
    // descriptor, rather than an earlier path lookup, determines the file kind.
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&paths.credentials)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Failure::Error(paths::node_fs_message_or_unknown(
                &error,
                "open",
                &paths.credentials_label,
            ))
            .into());
        }
    };
    let metadata = file.metadata().map_err(|error| {
        Failure::Error(paths::node_fs_message_or_unknown(
            &error,
            "fstat",
            &paths.credentials_label,
        ))
    })?;
    if !metadata.is_file() {
        return Err(Failure::Error("Refusing non-regular Weixin credential file".into()).into());
    }
    if metadata.len() > CREDENTIAL_LIMIT {
        return Err(Failure::Error("Weixin credentials exceed 1048576-byte limit".into()).into());
    }
    let mut bytes = Vec::new();
    file.take(CREDENTIAL_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            Failure::Error(paths::node_fs_message_or_unknown(
                &error,
                "read",
                &paths.credentials_label,
            ))
        })?;
    if bytes.len() as u64 > CREDENTIAL_LIMIT {
        return Err(Failure::Error("Weixin credentials exceed 1048576-byte limit".into()).into());
    }
    let text = String::from_utf8_lossy(&bytes);
    let Some(value) = parse_json(&text).ok() else {
        return Ok(None);
    };
    Ok(["baseUrl", "botToken", "accountId"]
        .iter()
        .all(|key| config::truthy(config::get(&value, &[key])))
        .then_some(value))
}
fn enable(paths: &SetupPaths) -> SetupResult<()> {
    config::write_adapter(
        &paths.config,
        "weixin",
        config::object([("enabled", JsValue::Bool(true))]),
    )
}
fn save_credentials(paths: &SetupPaths, credentials: &Credentials) -> SetupResult<()> {
    let (home, label) = paths.weixin_home()?;
    fs::create_dir_all(&home).map_err(|error| {
        Failure::Error(paths::node_fs_message_or_unknown(&error, "mkdir", &label))
    })?;
    // The baseline creates os.homedir()/.pilotdeck even when PILOT_HOME points
    // elsewhere. Preserve its missing custom-parent failure before preparing
    // the atomic write; do not create an unrelated parent as an incidental fix.
    let parent = paths
        .credentials
        .parent()
        .expect("credential path has parent");
    match fs::metadata(parent) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(Failure::Error(paths::node_fs_message_or_unknown(
                &std::io::Error::from_raw_os_error(libc::ENOTDIR),
                "open",
                &paths.credentials_label,
            ))
            .into());
        }
        Err(error) => {
            return Err(Failure::Error(paths::node_fs_message_or_unknown(
                &error,
                "open",
                &paths.credentials_label,
            ))
            .into());
        }
    }
    let value = config::object([
        ("baseUrl", credentials.base.clone()),
        ("botToken", credentials.token.clone()),
        ("accountId", credentials.account.clone()),
    ]);
    let raw = stringify_json(&value)
        .map_err(|error| Failure::Error(error.to_string()))?
        .expect("credential object has JSON");
    save::Target::prepare(&paths.credentials)?.publish(crate::json_pretty(&raw).as_bytes())
}
pub(super) fn setup(
    paths: &SetupPaths,
    io: &mut dyn Prompt,
    network: &mut dyn Fetch,
) -> SetupResult<()> {
    io.line("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")?;
    io.line("  微信 iLink 配置")?;
    io.line("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n")?;
    if let Some(credentials) = load(paths)? {
        io.line(&format!(
            "已有凭据 (accountId: {})",
            config::text(config::get(&credentials, &["accountId"]))
        ))?;
        if !matches!(io.ask("重新登录？ [y/N]: ")?.as_str(), "y" | "yes") {
            enable(paths)?;
            io.line("✅ 微信已启用（使用现有凭据）\n")?;
            return Ok(());
        }
    }
    io.line("⚠️  注意：")?;
    io.line("  - 使用 iLink Bot API，非企业微信")?;
    io.line("  - 建议使用小号登录，有封号风险")?;
    io.line("  - 群聊功能默认禁用\n")?;
    if matches!(io.ask("继续扫码登录？ [Y/n]: ")?.as_str(), "n" | "no") {
        io.line("已取消。")?;
        return Ok(());
    }
    io.settle();
    io.line("\n╔══════════════════════════════════════════════╗")?;
    io.line("║  请用微信扫描二维码                          ║")?;
    io.line("╚══════════════════════════════════════════════╝\n")?;
    let result = login(io, network);
    match result {
        Ok(credentials) => {
            if let Err(error) = save_credentials(paths, &credentials).and_then(|()| enable(paths)) {
                let message = match error {
                    super::prompt::SetupError::Failure(Failure::Error(message)) => message,
                    super::prompt::SetupError::Failure(Failure::Todo(message)) => {
                        format!("pilotdeck-rs TODO: {message} is not implemented yet.")
                    }
                    super::prompt::SetupError::Eof => return Err(super::prompt::SetupError::Eof),
                };
                return login_failed(io, &message);
            }
            io.line("\n✅ 微信登录成功！")?;
            io.line(&format!(
                "   账号 ID: {}",
                config::text(&credentials.account)
            ))?;
            io.line(&format!(
                "   凭据已保存到: {}",
                paths.credentials_label.display()
            ))?;
            io.line("   重启 PilotDeck 服务后生效\n")
        }
        Err(error) => login_failed(io, &error),
    }
}
fn login_failed(io: &mut dyn Prompt, message: &str) -> SetupResult<()> {
    io.line(&format!("\n❌ 微信登录失败: {message}"))?;
    io.line("   请检查网络连接后重试。\n")
}
fn show_qr(io: &mut dyn Prompt, value: &JsValue) -> Result<(), String> {
    let text = config::text(value);
    io.line("扫码登录链接：")
        .map_err(|error| format!("{error:?}"))?;
    io.line(&format!("  {text}\n"))
        .map_err(|error| format!("{error:?}"))?;
    io.line("如果终端无法显示二维码，请在浏览器中打开上述链接。\n")
        .map_err(|error| format!("{error:?}"))
}
fn status(io: &mut dyn Prompt, value: &str) -> Result<(), String> {
    let text = match value {
        "waiting" => "⏳ 等待扫码...",
        "scanned" => "📱 已扫码，等待确认...",
        "expired" => "⏰ 二维码已过期，正在刷新...",
        "refreshing" => "🔄 刷新中...",
        _ => {
            return io
                .line(&format!("状态: {value}"))
                .map_err(|error| format!("{error:?}"));
        }
    };
    io.line(text).map_err(|error| format!("{error:?}"))
}
fn property<'a>(value: &'a JsValue, key: &str) -> Result<&'a JsValue, String> {
    if matches!(value, JsValue::Null | JsValue::Undefined) {
        return Err(format!(
            "Cannot read properties of {} (reading '{key}')",
            config::text(value)
        ));
    }
    Ok(config::get(value, &[key]))
}
fn get_qr(network: &mut dyn Fetch, base: &str) -> Result<JsValue, String> {
    let response = network
        .ilink_json(
            &format!("{base}/ilink/bot/get_bot_qrcode?bot_type=3"),
            false,
        )
        .map_err(message)?;
    if !(200..300).contains(&response.status) {
        return Err(format!("Failed to fetch QR code: {}", response.status));
    }
    Ok(response.json)
}
fn message(error: IlinkFailure) -> String {
    match error {
        IlinkFailure::Abort => "This operation was aborted".into(),
        IlinkFailure::Error(error) => error,
    }
}
fn login(io: &mut dyn Prompt, network: &mut dyn Fetch) -> Result<Credentials, String> {
    let base = pilotdeck_gateway::channel::weixin::default_login_base_url();
    let mut qr = get_qr(network, &base)?;
    let mut refresh_count = 1;
    show_qr(io, property(&qr, "qrcode_img_content")?)?;
    status(io, "waiting")?;
    let deadline = network.now_ms().saturating_add(LOGIN_MS);
    while network.now_ms() < deadline {
        let code =
            super::wecom::component(&super::wecom::js_string(config::get(&qr, &["qrcode"])))?;
        let response = match network.ilink_json(
            &format!("{base}/ilink/bot/get_qrcode_status?qrcode={code}"),
            true,
        ) {
            Ok(response) => response,
            Err(IlinkFailure::Abort) => {
                network.sleep(POLL_MS);
                continue;
            }
            Err(error) => return Err(message(error)),
        };
        if !(200..300).contains(&response.status) {
            return Err(format!("QR status poll failed: {}", response.status));
        }
        let value = &response.json;
        let state = property(value, "status")?;
        if state
            .as_string()
            .is_some_and(|state| state.equals_str("scaned"))
        {
            status(io, "scanned")?;
        } else if state
            .as_string()
            .is_some_and(|state| state.equals_str("expired"))
        {
            refresh_count += 1;
            if refresh_count > 3 {
                return Err("QR code expired too many times".into());
            }
            status(io, "refreshing")?;
            qr = get_qr(network, &base)?;
            show_qr(io, property(&qr, "qrcode_img_content")?)?;
            status(io, "waiting")?;
        } else if state
            .as_string()
            .is_some_and(|state| state.equals_str("confirmed"))
        {
            let token = config::get(value, &["bot_token"]);
            let account = config::get(value, &["ilink_bot_id"]);
            if !config::truthy(token) || !config::truthy(account) {
                return Err("Login failed: server did not return required credentials".into());
            }
            let returned = config::get(value, &["baseurl"]);
            return Ok(Credentials {
                token: token.clone(),
                account: account.clone(),
                base: if config::truthy(returned) {
                    returned.clone()
                } else {
                    config::string(&base)
                },
            });
        }
        network.sleep(POLL_MS);
    }
    Err("Login timed out".into())
}
