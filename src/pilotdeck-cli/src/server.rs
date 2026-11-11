//! Legacy server argv/env selection over the one Rust host lifecycle.
use crate::{
    Failure,
    paths::{self, Env},
};
use pilotdeck_cli::{
    management::{HttpRuntime, RestartManager},
    update::Installation,
};
use pilotdeck_gateway::native_process::{self, HostExit, ServerOptions};
use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    sync::Arc,
};
fn fail(error: impl std::fmt::Display) -> Failure {
    Failure::Error(error.to_string())
}
fn string_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|v| v == flag)
        .and_then(|i| args.get(i + 1))
        .filter(|v| !v.is_empty() && !v.starts_with("--"))
        .map(String::as_str)
}
fn integer_prefix(value: &str) -> Option<f64> {
    let value = paths::js_trim(value);
    let digits = value
        .strip_prefix(['+', '-'])
        .unwrap_or(value)
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits == 0 {
        return None;
    }
    value[..digits + usize::from(value.starts_with(['+', '-']))]
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}
fn port(args: &[String], env: &Env) -> Result<u16, Failure> {
    // The fixed CLI takes the first flag and JS parseInt's decimal prefix.
    let flag = args
        .iter()
        .position(|s| s == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| integer_prefix(s));
    let value = flag
        .or_else(|| {
            env.string("PILOTDECK_GATEWAY_PORT")
                .and_then(|v| integer_prefix(&v))
        })
        .unwrap_or(18789.0);
    if !value.is_finite() || !(0.0..=65535.0).contains(&value) {
        return Err(fail(format!(
            "Invalid Gateway port: {}",
            pilotdeck_protocol::number::number_to_string(value)
        )));
    }
    Ok(value as u16)
}
fn explicit_token(env: &Env) -> Option<String> {
    env.string("PILOTDECK_GATEWAY_TOKEN")
        .filter(|v| !v.is_empty())
        .or_else(|| env.string("PILOTDECK_TOKEN").filter(|v| !v.is_empty()))
}
fn auth(env: &Env, cwd: &Path, create: bool) -> Result<(String, Option<PathBuf>), Failure> {
    if let Some(token) = explicit_token(env) {
        return Ok((token, None));
    }
    let path = paths::gateway_token_path(env, cwd)?;
    match super::read_tui_token(&path) {
        Ok(bytes) => {
            let value = String::from_utf8_lossy(&bytes);
            let token = paths::js_trim(&value);
            if !token.is_empty() {
                return Ok((token.into(), Some(path)));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(fail(e)),
    }
    if !create {
        return Err(fail("Gateway authentication token is unavailable."));
    }
    let parent = path
        .parent()
        .ok_or_else(|| fail("Gateway token has no parent directory."))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(fail)?;
    let mut randomness = [0_u8; 32];
    use std::io::Read;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut randomness))
        .map_err(fail)?;
    // Same 32 bytes / base64url-without-padding token format as the old host.
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut token = String::new();
    for chunk in randomness.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..chunk.len() + 1 {
            token.push(alphabet[((bits >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    let target = pilotdeck_cli::atomic_file::Target::prepare(&path).map_err(fail)?;
    // Another starter may have published between the first read and prepare.
    // Reuse it; publish() also verifies its captured identity before replacement.
    if let Ok(bytes) = super::read_tui_token(&path) {
        let value = String::from_utf8_lossy(&bytes);
        let existing = paths::js_trim(&value);
        if !existing.is_empty() {
            return Ok((existing.into(), Some(path)));
        }
    }
    target
        .publish(format!("{token}\n").as_bytes())
        .map_err(fail)?;
    Ok((token, Some(path)))
}

fn remote_policy(bind_host: &str, env: &Env) -> Result<bool, Failure> {
    let allow_remote = env.string("PILOTDECK_GATEWAY_ALLOW_REMOTE").as_deref() == Some("1");
    if !pilotdeck_gateway::native_process::legacy_local_bind_host(bind_host) {
        if !allow_remote {
            return Err(fail(
                "GatewayServer remote binding requires allowRemoteHost=true.",
            ));
        }
        if !explicit_token(env).is_some_and(|s| !paths::js_trim(&s).is_empty()) {
            return Err(fail(
                "GatewayServer remote binding requires an explicit auth token.",
            ));
        }
    }
    Ok(allow_remote)
}

pub fn run(args: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let core = super::local_host_options(env, cwd)?;
    super::config::check(env, cwd, &core.home)?;
    super::prepare_legacy_skills(env, cwd, &core.home)?;
    let bind_host = string_flag(args, "--host")
        .map(str::to_owned)
        .or_else(|| env.string("PILOTDECK_GATEWAY_HOST"))
        .unwrap_or_else(|| "127.0.0.1".into());
    let allow_remote = remote_policy(&bind_host, env)?;
    let (token, token_path) = auth(env, cwd, true)?;
    let (manager, restart) = match Installation::discover(&std::env::current_exe().map_err(fail)?) {
        Ok(installation) => {
            let (manager, receiver) = RestartManager::new(installation).map_err(fail)?;
            (Some(manager), Some(receiver))
        }
        Err(e) if e.reason == "development" => (None, None),
        Err(e) => return Err(fail(e)),
    };
    let label = bind_host.clone();
    let ready_manager = manager.clone();
    let options = ServerOptions {
        core,
        bind_host,
        port: port(args, env)?,
        allow_remote,
        explicit_token: explicit_token(env).is_some(),
        token,
        static_assets: Some(cwd.join("ui/dist")),
        control: manager.as_ref().map(|m| m.handler()),
        restart,
        ready: Arc::new(move |address: SocketAddr| {
            let manager = ready_manager.clone();
            let label = label.clone();
            let token_path = token_path.clone();
            Box::pin(async move {
                if let Some(manager) = manager {
                    tokio::task::spawn_blocking(move || manager.publish_address(address))
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                }
                crate::stdout_line(&format!(
                    "PilotDeck server listening: http://{label}:{}",
                    address.port()
                ));
                crate::stdout_line(&format!("WebSocket: ws://{label}:{}/ws", address.port()));
                if let Some(path) = token_path {
                    crate::stdout_line(&format!("Token: {}", path.display()));
                }
                Ok(())
            })
        }),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(fail)?;
    let result = runtime
        .block_on(native_process::run_server(options, env.strings()))
        .map_err(fail)?;
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    if result == HostExit::Restart {
        let executable = manager
            .ok_or_else(|| fail("No installation owns this restart."))?
            .restart_executable()
            .map_err(fail)?;
        let error = std::process::Command::new(executable)
            .arg("server")
            .args(args)
            .current_dir(cwd)
            .exec();
        return Err(fail(format!(
            "Update prepared but restarting the executable failed: {error}. Restart manually."
        )));
    }
    Ok(0)
}

pub fn update(args: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    // Flag validation and the source-installation check precede any runtime IO.
    let flags = pilotdeck_cli::update::Flags::parse(args).map_err(fail)?;
    if !flags.restart || flags.check_only {
        return pilotdeck_cli::update::run_current(args, None).map_err(fail);
    }
    // A missing token cannot mean "no server": authenticate the running owner.
    let installation =
        Installation::discover(&std::env::current_exe().map_err(fail)?).map_err(fail)?;
    let location = pilotdeck_cli::runtime_record::discover(&installation).map_err(fail)?;
    let (token, _) = auth(env, cwd, false)?;
    let mut client = HttpRuntime {
        address: location.address,
        instance_id: location.instance_id,
        token,
    };
    pilotdeck_cli::update::run_current(args, Some(&mut client)).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_uses_first_decimal_flag_and_falls_back_to_environment() {
        let env = Env::from_pairs(&[("PILOTDECK_GATEWAY_PORT", "25000tail")]);
        assert_eq!(
            port(
                &[
                    "--port".into(),
                    "12345x".into(),
                    "--port".into(),
                    "5".into()
                ],
                &env
            )
            .unwrap(),
            12345
        );
        assert_eq!(port(&["--port".into(), "no".into()], &env).unwrap(), 25000);
        assert_eq!(port(&["--port".into(), "0".into()], &env).unwrap(), 0);
        assert!(port(&["--port".into(), "-1".into()], &env).is_err());
        assert!(port(&["--port".into(), "99999999999999999999".into()], &env).is_err());
        assert!(port(&["--port".into(), "65536".into()], &env).is_err());
        assert_eq!(
            port(&["--port".into(), "65535".into()], &env).unwrap(),
            65535
        );
        assert_eq!(
            port(&["--port".into(), "9".repeat(400)], &env).unwrap(),
            25000
        );
        assert_eq!(port(&["--port".into(), "١٢٣".into()], &env).unwrap(), 25000);

        assert_eq!(
            string_flag(&["--host".into(), "--port".into()], "--host"),
            None
        );
    }
    #[test]
    fn remote_bind_requires_exact_opt_in_and_an_explicit_nonblank_token() {
        for flag in ["", "true", "0"] {
            let env = Env::from_pairs(&[
                ("PILOTDECK_GATEWAY_ALLOW_REMOTE", flag),
                ("PILOTDECK_GATEWAY_TOKEN", "explicit"),
            ]);
            assert!(remote_policy("0.0.0.0", &env).is_err());
        }
        for token in ["", " ", "\u{feff}"] {
            let env = Env::from_pairs(&[
                ("PILOTDECK_GATEWAY_ALLOW_REMOTE", "1"),
                ("PILOTDECK_GATEWAY_TOKEN", token),
            ]);
            assert!(remote_policy("0.0.0.0", &env).is_err());
        }
        let env = Env::from_pairs(&[
            ("PILOTDECK_GATEWAY_ALLOW_REMOTE", "1"),
            ("PILOTDECK_GATEWAY_TOKEN", "explicit"),
        ]);
        assert!(remote_policy("0.0.0.0", &env).unwrap());
        let empty = Env::from_pairs(&[]);
        assert!(!remote_policy("localhost", &empty).unwrap());
        assert!(!remote_policy("127.0.0.1", &empty).unwrap());
    }
}
