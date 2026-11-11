//! `pilotdeck` entry. Dispatch order follows src/cli/pilotdeck.ts main().
mod chat_search;
mod cli_channel;
mod config;
mod cron;
mod gateway_probe;
mod gateway_setup;
pub(crate) mod paths;
pub(crate) mod sdk;
mod server;
pub(crate) mod skills;
mod skills_migrate;
#[cfg(test)]
#[path = "../tests/common/negative_receipt.rs"]
pub(crate) mod test_negative_receipt;

use paths::Env;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

#[derive(Debug)]
pub enum Failure {
    /// A thrown Error: TS main().catch prints its message and sets exit code 1.
    Error(String),
    /// A branch the Rust port has not reached yet. Never a parity result.
    Todo(&'static str),
}

// The String form of the reindentation in write_json_pretty_into, its bytes
// collected in memory. One implementation only (#983 fix-r11): sessions list
// and settings stream through the writer directly, so a second copy of the
// indent/escape logic here could drift unmutated past the negative controls.
pub(crate) fn json_pretty(raw: &str) -> String {
    let mut output = Vec::new();
    write_json_pretty_into(raw, &mut output).expect("#983 in-memory pretty write");
    String::from_utf8(output).expect("#983 pretty bytes are valid UTF-8")
}

pub fn stdout_line(text: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

// json_pretty's reindentation written straight to stdout in bounded chunks: a
// sessions projection can be the whole response frame, and building the full
// pretty String first would hold a second copy of it in memory (#983). The
// bytes and the ignored-write-error policy match `stdout_line(&json_pretty(raw))`.
pub fn write_json_pretty_line(raw: &str) {
    let _ = try_write_json_pretty_line(raw);
}

fn try_write_json_pretty_line(raw: &str) -> std::io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, stdout.lock());
    write_json_pretty_into(raw, &mut out)?;
    out.write_all(b"\n")?;
    out.flush()
}

// Reindent bytes from the JS codec, preserving property order, number spelling,
// lone surrogates and all whitespace inside strings. Callers own codec errors
// and the policy for values without a JSON representation.
fn write_json_pretty_into(raw: &str, out: &mut impl Write) -> std::io::Result<()> {
    fn char(out: &mut impl Write, value: char) -> std::io::Result<()> {
        let mut encoded = [0_u8; 4];
        out.write_all(value.encode_utf8(&mut encoded).as_bytes())
    }

    fn newline(out: &mut impl Write, depth: usize) -> std::io::Result<()> {
        out.write_all(b"\n")?;
        for _ in 0..depth {
            out.write_all(b"  ")?;
        }
        Ok(())
    }

    let mut chars = raw.chars().peekable();
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    while let Some(value) = chars.next() {
        if quoted {
            char(out, value)?;
            if escaped {
                escaped = false;
            } else if value == '\\' {
                escaped = true;
            } else if value == '"' {
                quoted = false;
            }
            continue;
        }
        match value {
            '"' => {
                quoted = true;
                char(out, value)?;
            }
            '{' | '[' => {
                char(out, value)?;
                let close = if value == '{' { '}' } else { ']' };
                if chars.peek() == Some(&close) {
                    char(out, close)?;
                    chars.next();
                } else {
                    depth += 1;
                    newline(out, depth)?;
                }
            }
            '}' | ']' => {
                depth -= 1;
                newline(out, depth)?;
                char(out, value)?;
            }
            ',' => {
                char(out, value)?;
                newline(out, depth)?;
            }
            ':' => out.write_all(b": ")?,
            _ => char(out, value)?,
        }
    }
    Ok(())
}

pub fn stderr_line(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

fn main() -> ExitCode {
    if let Some(code) =
        pilotdeck_gateway::native_installation_owner_helper(std::env::args_os().skip(1))
    {
        return ExitCode::from(code as u8);
    }
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let env = Env::from_process();
    let code = match std::env::current_dir() {
        Ok(cwd) => run(&argv, &env, &cwd),
        Err(error) => Err(Failure::Error(
            pilotdeck_gateway::process_cwd_diagnostic(&error).1,
        )),
    };
    ExitCode::from(match code {
        Ok(code) => code,
        Err(Failure::Error(message)) => {
            stderr_line(&message);
            1
        }
        Err(Failure::Todo(what)) => {
            stderr_line(&format!(
                "pilotdeck-rs TODO: {what} is not implemented yet."
            ));
            2
        }
    })
}

pub(crate) fn run(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let command = argv.first().map(String::as_str);
    match command {
        Some("run" | "resume" | "sessions" | "settings" | "help" | "--help" | "-h") => {
            sdk::run_sdk_cli(argv, env, cwd)
        }
        Some("server") => server::run(&argv[1..], env, cwd),
        Some("gateway") => {
            if argv.get(1).map(String::as_str) == Some("setup") {
                return gateway_setup::run(&argv[2..], env, cwd);
            }
            stderr_line("Usage: pilotdeck gateway setup [feishu|weixin|wecom]");
            Ok(1)
        }
        Some("cron") => cron::run(&argv[1..], env, cwd),
        Some("update") => server::update(&argv[1..], env, cwd),
        Some("skills") => skills_migrate::run(&argv[1..], env, cwd),
        Some("chat") => chat_search::run(&argv[1..], env, cwd),
        Some("tui") => {
            if !std::io::stdin().is_terminal() {
                tui_stderr_nonblocking("pilotdeck tui requires an interactive terminal.");
                return Ok(1);
            }
            match run_tui(env, cwd) {
                Ok(code) => Ok(code),
                Err(Failure::Error(message)) => {
                    tui_stderr_nonblocking(&message);
                    Ok(1)
                }
                Err(Failure::Todo(what)) => {
                    tui_stderr_nonblocking(&format!(
                        "pilotdeck-rs TODO: {what} is not implemented yet."
                    ));
                    Ok(2)
                }
            }
        }
        _ => run_local_cli_channel(argv, env, cwd),
    }
}

fn tui_stderr_nonblocking(message: &str) {
    let stderr = std::io::stderr();
    if let Ok(flags) = rustix::fs::fcntl_getfl(&stderr)
        && rustix::fs::fcntl_setfl(&stderr, flags | rustix::fs::OFlags::NONBLOCK).is_ok()
    {
        let _ = writeln!(stderr.lock(), "{message}");
        let _ = rustix::fs::fcntl_setfl(&stderr, flags);
    }
}

fn local_host_options(
    env: &Env,
    cwd: &Path,
) -> Result<pilotdeck_gateway::native_process::LocalHostOptions, Failure> {
    let home = paths::resolve(cwd, &paths::pilot_home(env, cwd)?);
    let sidecar = match env.string("PILOTDECK_RS_SIDECAR") {
        Some(path) if !path.trim().is_empty() => paths::resolve(cwd, Path::new(&path)),
        _ => std::env::current_exe()
            .map_err(|e| Failure::Error(e.to_string()))?
            .parent()
            .ok_or_else(|| Failure::Error("CLI executable has no parent directory".into()))?
            .join("pilotdeck-agent-loop-sidecar"),
    };
    Ok(pilotdeck_gateway::native_process::LocalHostOptions {
        home,
        project: cwd.to_path_buf(),
        sidecar,
        sidecar_args: Vec::new(),
        drain_budget: std::time::Duration::from_secs(10),
    })
}

fn run_tui(env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let home = paths::resolve(cwd, &paths::pilot_home(env, cwd)?);
    #[cfg(unix)]
    let load_snapshot = pilotdeck_gateway::config::load_snapshot_bounded;
    #[cfg(not(unix))]
    let load_snapshot = pilotdeck_gateway::config::load_snapshot;
    let snapshot =
        load_snapshot(&home, cwd, &env.strings(), Some("."), 1.0, "").map_err(|error| {
            if error.requires_host {
                Failure::Todo("config requiring JavaScript host support")
            } else {
                Failure::Error(String::from_utf16_lossy(error.message.units()))
            }
        })?;
    let port = snapshot
        .get("config")
        .and_then(|v| v.get("gateway"))
        .and_then(|v| v.get("port"))
        .and_then(|v| v.as_number())
        .unwrap_or(18789.0);
    if !port.is_finite() || port.fract() != 0.0 || !(1.0..=65535.0).contains(&port) {
        return Err(Failure::Error("Invalid gateway port.".into()));
    }
    let base = format!("http://127.0.0.1:{}", port as u16);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::Error(e.to_string()))?;
    runtime.block_on(async {
        if gateway_probe::healthy_at(&base).await {
            let token = match paths::gateway_token_path(env, cwd) {
                Ok(path) => sdk::token::read(&path)?,
                Err(_) => String::new(),
            };
            if !token.is_empty() {
                let url = format!("ws://127.0.0.1:{}/ws", port as u16);
                if let Ok(client) =
                    pilotdeck_cli::tui::transport::Client::connect(&url, &token).await
                {
                    return pilotdeck_cli::tui::runner::run_with_client(client, &url, &home, cwd)
                        .await
                        .map_err(Failure::Error);
                }
            }
        }
        let options = local_host_options(env, cwd)?;
        let client = pilotdeck_cli::tui::transport::Client::local(options, env.strings())
            .await
            .map_err(|error| Failure::Error(format!("Local Gateway failed: {error:?}")))?;
        pilotdeck_cli::tui::runner::run_with_client(client, "local", &home, cwd)
            .await
            .map_err(Failure::Error)
    })
}

// The server's existing-file reader deliberately returns raw bytes and rejects
// directories: a failed server credential read must not mint a replacement.
// Discovery clients use sdk::token::read and retain their ordinary-IO fallback.
fn read_tui_token(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "Gateway token must be a regular file.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(std::io::Error::other("Gateway token is too large."));
    }
    Ok(bytes)
}

/// createLocalGateway() up to its first observable effects, then CliChannel.
fn prepare_legacy_skills(env: &Env, cwd: &Path, pilot_home: &Path) -> Result<(), Failure> {
    // Node's import.meta.url is the realpath of the entry module; match it so a
    // symlinked launcher still probes beside the real install root.
    let exe = std::env::current_exe()
        .ok()
        .map(|exe| std::fs::canonicalize(&exe).unwrap_or(exe));
    let exe_dir = exe.as_deref().and_then(Path::parent);
    let builtin_root = skills::builtin_skills_root(env, cwd, exe_dir)?;
    let migration = skills::migrate_legacy_bundled_skill_copies(pilot_home, &builtin_root, cwd)?;
    if let Some(notice) = migration.notice {
        stdout_line(&notice);
    }
    for warning in migration.warnings {
        stderr_line(&warning);
    }
    Ok(())
}

fn run_local_cli_channel(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let pilot_home = paths::pilot_home(env, cwd)?;
    prepare_legacy_skills(env, cwd, &pilot_home)?;
    config::check(env, cwd, &pilot_home)?;
    cli_channel::run(argv, env, cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_negative_receipt::verify;

    /// #983: the streamed pretty printer must be byte-identical to the
    /// two-String form it replaces, including escapes, lone surrogates,
    /// non-ASCII text, empty containers and deep nesting.
    #[test]
    fn streamed_pretty_matches_json_pretty_bytes() {
        for raw in [
            "[]",
            "{}",
            "0",
            "[[[[1]]]]",
            r#"[{"z":[{},[],"中\"[,]"],"a":-0,"b":1e+21,"s":"\ud800"}]"#,
            r#"{"a":{"b":[1,2]},"c":"}\"\"","d":"中"}"#,
            r#"[true,false,null,"\n\t\r\b\f/"]"#,
            r#"{"nested":{"empty":{},"list":[[],{},"x"]}}"#,
        ] {
            let mut streamed = Vec::new();
            write_json_pretty_into(raw, &mut streamed).expect("#983 streamed pretty write");
            verify(
                streamed == json_pretty(raw).as_bytes(),
                "#983 streamed pretty bytes",
            );
        }
    }
}

#[cfg(test)]
mod tui_token_tests {
    use super::read_tui_token;
    use std::fs;

    #[test]
    fn token_reads_are_bounded_and_do_not_block_on_fifo() {
        let dir = std::env::temp_dir().join(format!("pdrs-tui-token-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("token");
        fs::write(&path, b"fixture-token\n").unwrap();
        assert_eq!(read_tui_token(&path).unwrap(), b"fixture-token\n");
        fs::write(&path, vec![b'x'; 65537]).unwrap();
        assert!(read_tui_token(&path).is_err());
        fs::remove_file(&path).unwrap();
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(read_tui_token(&path).is_err());
        fs::remove_file(&path).unwrap();
        assert!(read_tui_token(&dir).is_err());
        fs::remove_dir(&dir).unwrap();
    }
}
