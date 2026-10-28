//! `pilotdeck` entry. Dispatch order follows src/cli/pilotdeck.ts main().
mod chat_search;
mod config;
mod paths;
mod sdk;
mod skills;
mod skills_migrate;

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

// Reindent bytes from the JS codec, preserving property order, number spelling,
// lone surrogates and all whitespace inside strings. Callers own codec errors
// and the policy for values without a JSON representation.
fn json_pretty(raw: &str) -> String {
    let mut chars = raw.chars().peekable();
    let mut output = String::new();
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    let newline = |output: &mut String, depth| {
        output.push('\n');
        output.push_str(&"  ".repeat(depth));
    };
    while let Some(ch) = chars.next() {
        if quoted {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                output.push(ch);
            }
            '{' | '[' => {
                output.push(ch);
                let close = if ch == '{' { '}' } else { ']' };
                if chars.peek() == Some(&close) {
                    output.push(close);
                    chars.next();
                } else {
                    depth += 1;
                    newline(&mut output, depth);
                }
            }
            '}' | ']' => {
                depth -= 1;
                newline(&mut output, depth);
                output.push(ch);
            }
            ',' => {
                output.push(ch);
                newline(&mut output, depth);
            }
            ':' => output.push_str(": "),
            _ => output.push(ch),
        }
    }
    output
}

pub fn stdout_line(text: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

pub fn stderr_line(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let env = Env::from_process();
    let code = match std::env::current_dir() {
        Ok(cwd) => run(&argv, &env, &cwd),
        Err(_) => Err(Failure::Todo(
            "startup without a readable working directory",
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

fn run(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let command = argv.first().map(String::as_str);
    match command {
        Some("run" | "resume" | "sessions" | "settings" | "help" | "--help" | "-h") => {
            sdk::run_sdk_cli(argv, env, cwd)
        }
        Some("server") => Err(Failure::Todo("pilotdeck server")),
        Some("gateway") => {
            if argv.get(1).map(String::as_str) == Some("setup") {
                return Err(Failure::Todo("pilotdeck gateway setup"));
            }
            stderr_line("Usage: pilotdeck gateway setup [feishu|weixin|wecom]");
            Ok(1)
        }
        Some("cron") => Err(Failure::Todo("pilotdeck cron")),
        Some("update") => Err(Failure::Todo("pilotdeck update")),
        Some("skills") => skills_migrate::run(&argv[1..], env, cwd),
        Some("chat") => chat_search::run(&argv[1..], env, cwd),
        Some("tui") => {
            if !std::io::stdin().is_terminal() {
                stderr_line("pilotdeck tui requires an interactive terminal.");
                return Ok(1);
            }
            Err(Failure::Todo("pilotdeck tui"))
        }
        _ => run_local_cli_channel(env, cwd),
    }
}

/// createLocalGateway() up to its first observable effects, then CliChannel.
fn run_local_cli_channel(env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let pilot_home = paths::pilot_home(env, cwd)?;
    // Node's import.meta.url is the realpath of the entry module; match it so a
    // symlinked launcher still probes beside the real install root.
    let exe = std::env::current_exe()
        .ok()
        .map(|exe| std::fs::canonicalize(&exe).unwrap_or(exe));
    let exe_dir = exe.as_deref().and_then(Path::parent);
    let builtin_root = skills::builtin_skills_root(env, cwd, exe_dir)?;
    let migration = skills::migrate_legacy_bundled_skill_copies(&pilot_home, &builtin_root)?;
    if let Some(notice) = migration.notice {
        stdout_line(&notice);
    }
    for warning in migration.warnings {
        stderr_line(&warning);
    }
    config::check(env, cwd, &pilot_home)?;
    Err(Failure::Todo(
        "local Gateway and CliChannel for a valid config",
    ))
}
