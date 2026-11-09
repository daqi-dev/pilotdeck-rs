//! The legacy cron CLI uses the local server discovery path, not SDK options.
use super::sdk::transport::{close_socket, websocket_config};
use crate::{
    Failure,
    paths::{self, Env},
    stderr_line, write_json_pretty_line,
};
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use serde_json::{Map, Value, json};
use std::{path::Path, time::Duration};
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::{connect_async_with_config, tungstenite::Message};

const UNAVAILABLE: &str = "pilotdeck cron requires a running pilotdeck server.";

pub(crate) fn run(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::Error(e.to_string()))?;
    runtime.block_on(run_async(argv, env, cwd))
}

async fn run_async(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    if !super::gateway_probe::healthy().await {
        stderr_line(UNAVAILABLE);
        return Ok(1);
    }
    let token = match paths::gateway_token_path(env, cwd) {
        Ok(path) => super::sdk::token::read(&path)?,
        Err(_) => String::new(),
    };
    if token.is_empty() {
        stderr_line(UNAVAILABLE);
        return Ok(1);
    }
    // GatewayWsClient waits for WS open before starting its hello timer.
    let Ok((mut ws, _)) =
        connect_async_with_config("ws://127.0.0.1:18789/ws", Some(websocket_config()), false).await
    else {
        stderr_line(UNAVAILABLE);
        return Ok(1);
    };
    if ws
        .send(Message::Text(
            json!({"type":"hello", "protocolVersion":pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,
            "clientName":"cli", "clientVersion":"0.1.0", "token":token})
            .to_string()
            .into(),
        ))
        .await
        .is_err()
    {
        stderr_line(UNAVAILABLE);
        return Ok(1);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = timeout_at(deadline, async {
        loop {
            // Ready frames can otherwise monopolize a single poll past the deadline.
            if Instant::now() >= deadline {
                return None;
            }
            let frame = timeout_at(deadline, ws.next()).await.ok()??.ok()?;
            if Instant::now() >= deadline {
                return None;
            }
            if frame.is_text() {
                let value: Value = serde_json::from_str(frame.to_text().ok()?).ok()?;
                if value.get("type").and_then(Value::as_str) == Some("hello_ok") {
                    return Some(ws);
                }
            }
            if frame.is_close() {
                return None;
            }
        }
    })
    .await
    .ok()
    .flatten();
    let Some(mut ws) = result else {
        stderr_line(UNAVAILABLE);
        return Ok(1);
    };
    let (method, params) = match command(argv, cwd) {
        Ok(value) => value,
        Err(usage) => {
            close_socket(&mut ws).await;
            stderr_line(usage);
            return Ok(1);
        }
    };
    let id = uuid::Uuid::new_v4().to_string();
    ws.send(Message::Text(
        json!({"type":"request", "id":id, "method":method, "params":params})
            .to_string()
            .into(),
    ))
    .await
    .map_err(|_| Failure::Error("Gateway WebSocket closed.".into()))?;
    while let Some(frame) = ws.next().await {
        let frame = frame.map_err(|_| Failure::Error("Gateway WebSocket closed.".into()))?;
        if frame.is_close() {
            break;
        }
        if !frame.is_text() {
            continue;
        }
        let Ok(value) = parse_json(frame.to_text().unwrap_or_default()) else {
            continue;
        };
        let matches = |field: &str, expected: &str| {
            value
                .get(field)
                .and_then(JsValue::as_string)
                .is_some_and(|v| v.equals_str(expected))
        };
        if !matches("type", "response") || !matches("id", &id) {
            continue;
        }
        if value.get("ok").and_then(JsValue::as_bool) == Some(true) {
            let undefined = JsValue::Undefined;
            let result = value.get("result").unwrap_or(&undefined);
            let raw = stringify_json(result)
                .map_err(|e| Failure::Error(e.to_string()))?
                .unwrap_or_else(|| "undefined".into());
            if raw == "undefined" {
                crate::stdout_line("undefined");
            } else {
                write_json_pretty_line(&raw);
            }
            close_socket(&mut ws).await;
            return Ok(0);
        }
        let message = value
            .get("error")
            .and_then(|v| v.get("message"))
            .and_then(JsValue::as_string)
            .map(|s| String::from_utf16_lossy(s.units()))
            .unwrap_or_else(|| "Gateway request failed.".into());
        close_socket(&mut ws).await;
        return Err(Failure::Error(message));
    }
    Err(Failure::Error("Gateway WebSocket closed.".into()))
}

fn flag<'a>(argv: &'a [String], name: &str) -> Option<&'a str> {
    argv.iter()
        .position(|v| v == name)
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
        .filter(|v| !v.is_empty() && !v.starts_with("--"))
}

// Number.parseInt(s, 10): leading whitespace and sign, then a decimal prefix.
fn integer(s: &str) -> Option<f64> {
    let s = s.trim_start_matches(|c: char| paths::js_trim(&c.to_string()).is_empty());
    let sign = if s.starts_with('-') { -1.0 } else { 1.0 };
    let digits = s
        .strip_prefix('-')
        .or_else(|| s.strip_prefix('+'))
        .unwrap_or(s);
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let value = digits.parse::<f64>().ok()? * sign;
    value.is_finite().then_some(value)
}

fn command(argv: &[String], cwd: &Path) -> Result<(&'static str, Value), &'static str> {
    let mut params = Map::new();
    match argv.first().map(String::as_str) {
        Some("list") => {
            params.insert(
                "includeHistory".into(),
                argv.iter().any(|v| v == "--history").into(),
            );
            if let Some(number) = flag(argv, "--limit").and_then(integer).and_then(|v| {
                serde_json::from_str::<serde_json::Number>(
                    &pilotdeck_protocol::number::number_to_string(v),
                )
                .ok()
            }) {
                params.insert("limit".into(), number.into());
            }
            Ok(("cron_list", params.into()))
        }
        Some("create") => {
            let (Some(message), Some(session)) = (flag(argv, "--message"), flag(argv, "--session"))
            else {
                return Err(
                    "Usage: pilotdeck cron create --session <sessionKey> --message <text> (--once <iso> | --cron <expr>)",
                );
            };
            let schedule = if let Some(once) = flag(argv, "--once") {
                json!({"type":"once", "runAt":once})
            } else if let Some(cron) = flag(argv, "--cron") {
                json!({"type":"cron", "expression":cron})
            } else {
                return Err(
                    "Usage: pilotdeck cron create --session <sessionKey> --message <text> (--once <iso> | --cron <expr>)",
                );
            };
            let channel = flag(argv, "--channel").unwrap_or_else(|| {
                session
                    .find(':')
                    .filter(|i| *i > 0)
                    .map_or("cli", |i| &session[..i])
            });
            params.insert("message".into(), message.into());
            params.insert("sessionKey".into(), session.into());
            params.insert("channelKey".into(), channel.into());
            params.insert(
                "projectKey".into(),
                flag(argv, "--project")
                    .map(str::to_owned)
                    .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
                    .into(),
            );
            params.insert("schedule".into(), schedule);
            if let Some(zone) = flag(argv, "--timezone") {
                params.insert("timezone".into(), zone.into());
            }
            Ok(("cron_create", params.into()))
        }
        Some("delete") => {
            let task = argv
                .get(1)
                .map(String::as_str)
                .or_else(|| flag(argv, "--task"));
            if task.is_none_or(str::is_empty) {
                return Err("Usage: pilotdeck cron delete <taskId> [--stop-running]");
            }
            params.insert("taskId".into(), task.unwrap_or_default().into());
            params.insert(
                "stopRunning".into(),
                argv.iter().any(|v| v == "--stop-running").into(),
            );
            Ok(("cron_delete", params.into()))
        }
        Some("stop") => {
            let task = argv
                .get(1)
                .map(String::as_str)
                .or_else(|| flag(argv, "--task"));
            let run = flag(argv, "--run");
            if task.is_none_or(str::is_empty) && run.is_none() {
                return Err(
                    "Usage: pilotdeck cron stop <taskId> or pilotdeck cron stop --run <runId>",
                );
            }
            if let Some(task) = task {
                params.insert("taskId".into(), task.into());
            }
            if let Some(run) = run {
                params.insert("runId".into(), run.into());
            }
            Ok(("cron_stop", params.into()))
        }
        _ => Err("Usage: pilotdeck cron <list|create|delete|stop>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn decimal_prefix_and_fallbacks_follow_legacy_cli() {
        assert_eq!(integer(" \t-12abc"), Some(-12.0));
        assert_eq!(integer("0x10"), Some(0.0));
        assert_eq!(integer("+x"), None);
        let (_, params) = command(
            &argv(&[
                "create",
                "--session",
                ":x",
                "--message",
                "m",
                "--cron",
                "*",
                "--once",
                "now",
            ]),
            Path::new("/project"),
        )
        .unwrap();
        assert_eq!(params["channelKey"], "cli");
        assert_eq!(params["schedule"], json!({"type":"once", "runAt":"now"}));
        let (_, params) = command(&argv(&["stop", "--run", "r"]), Path::new("/")).unwrap();
        assert_eq!(params["taskId"], "--run");
        assert_eq!(params["runId"], "r");
        assert!(command(&argv(&["create", "--session", "s"]), Path::new("/")).is_err());
    }
}
