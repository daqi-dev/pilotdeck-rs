//! Native `gateway setup`, from the fixed baseline's gatewaySetup.ts.
mod config;
mod feishu;
mod http;
mod prompt;
mod terminal;

use crate::{Failure, paths::Env};
use config::SetupPaths;
use prompt::{Console, Prompt, SetupError, SetupResult};
use std::path::Path;

pub(crate) fn run(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let paths = SetupPaths::new(env, cwd)?;
    let mut console = Console::new();
    let result = run_with(argv, &paths, &mut console, &mut http::Network::new());
    match result {
        Ok(()) | Err(SetupError::Eof) => Ok(0),
        Err(SetupError::Failure(error)) => Err(error),
    }
}

fn run_with(
    argv: &[String],
    paths: &SetupPaths,
    io: &mut dyn Prompt,
    network: &mut dyn http::Fetch,
) -> SetupResult<()> {
    io.line("\n╔══════════════════════════════════════════════════╗")?;
    io.line("║  PilotDeck Gateway Setup                        ║")?;
    io.line("║  配置 IM 平台连接                                ║")?;
    io.line("╚══════════════════════════════════════════════════╝\n")?;
    let platform = argv.first().map(|value| value.to_lowercase());
    match platform.as_deref() {
        Some("feishu" | "lark") => feishu::setup(paths, io, network),
        Some("weixin" | "wechat") => {
            Err(SetupError::Failure(Failure::Todo("gateway setup weixin")))
        }
        Some("wecom" | "work-weixin" | "workwechat") => {
            Err(SetupError::Failure(Failure::Todo("gateway setup wecom")))
        }
        _ => {
            let config = config::load(&paths.config);
            let enabled =
                |name: &str| config::truthy(config::get(&config, &["adapters", name, "enabled"]));
            io.line("可配置的平台：")?;
            io.line(&format!(
                "  1) 飞书 / Lark      [{}]",
                if enabled("feishu") {
                    "✅ 已启用"
                } else {
                    "未配置"
                }
            ))?;
            io.line(&format!(
                "  2) 微信 (iLink)     [{}]",
                if paths.credentials.exists() {
                    "✅ 已有凭据"
                } else {
                    "未配置"
                }
            ))?;
            io.line(&format!(
                "  3) 企业微信 WeCom   [{}]",
                if enabled("wecom") {
                    "✅ 已启用"
                } else {
                    "未配置"
                }
            ))?;
            io.line("  4) 全部配置")?;
            io.line("  q) 退出\n")?;
            match io.ask("请选择 [1/2/3/4/q]: ")?.as_str() {
                "1" | "feishu" => feishu::setup(paths, io, network),
                "2" | "weixin" => Err(SetupError::Failure(Failure::Todo("gateway setup weixin"))),
                "3" | "wecom" => Err(SetupError::Failure(Failure::Todo("gateway setup wecom"))),
                "4" | "all" => {
                    feishu::setup(paths, io, network)?;
                    io.line("")?;
                    Err(SetupError::Failure(Failure::Todo(
                        "gateway setup remaining platforms",
                    )))
                }
                _ => io.line("未选择任何平台，退出。"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
