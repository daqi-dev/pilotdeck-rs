//! runSdkCli from src/cli/sdkCli.ts: argument reading and pre-network checks.
use crate::paths::{self, Env};
use crate::{Failure, json_syntax, stdout_line};
use std::path::Path;

const VALUE_FLAGS: [&str; 6] = [
    "--gateway-url",
    "--auth-token",
    "--project",
    "--channel",
    "--model",
    "--permission-mode",
];

const HELP: &str = "PilotDeck SDK CLI

  pilotdeck run <prompt> [options]
  pilotdeck resume <session-id> <prompt> [options]
  pilotdeck sessions list [options]
  pilotdeck sessions messages <session-id> [options]
  pilotdeck settings get [options]
  pilotdeck settings set '<json>' [options]

Options:
  --gateway-url <url>       Gateway WebSocket URL
  --auth-token <token>      Gateway auth token
  --project <path>          Explicit Gateway project key (defaults to Gateway workspace)
  --channel <name>          Session channel (defaults to cli)
  --model <provider/model>  Session model override
  --permission-mode <mode>  Session permission mode
  --json                    Print the final run/result as JSON
  --stream-json             Print each run event as JSONL";

pub fn run_sdk_cli(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    match argv.first().map(String::as_str) {
        None | Some("" | "help" | "--help" | "-h") => {
            stdout_line(HELP);
            Ok(0)
        }
        Some("run" | "resume") => {
            read_connection(&argv[1..], env, cwd)?;
            Err(Failure::Todo("pilotdeck run/resume (SDK Gateway client)"))
        }
        Some("sessions") => {
            read_connection(&argv[1..], env, cwd)?;
            Err(Failure::Todo("pilotdeck sessions (SDK Gateway client)"))
        }
        Some("settings") => run_settings(&argv[1..], env, cwd),
        Some(_) => Err(Failure::Error(
            "Usage: pilotdeck <run|resume|sessions|settings> ...".into(),
        )),
    }
}

fn run_settings(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    read_connection(argv, env, cwd)?;
    match argv.first().map_or("get", String::as_str) {
        "get" => Err(Failure::Todo(
            "pilotdeck settings get (SDK resolveSettings)",
        )),
        "set" => {
            let rest = &argv[1..];
            let raw = read_value(rest, "--json")
                .map(str::to_owned)
                .unwrap_or_else(|| positional(rest).join(" "));
            if raw.is_empty() {
                return Err(Failure::Error(
                    "Usage: pilotdeck settings set '<json>'".into(),
                ));
            }
            if let Some(message) = json_syntax::parse_error(&raw) {
                return Err(Failure::Error(format!("Invalid settings JSON: {message}")));
            }
            Err(Failure::Todo("pilotdeck settings set (SDK updateSettings)"))
        }
        _ => Err(Failure::Error(
            "Usage: pilotdeck settings <get|set> ...".into(),
        )),
    }
}

/// Only the auth token is checked before any connection is attempted.
fn read_connection(argv: &[String], env: &Env, cwd: &Path) -> Result<(), Failure> {
    let token = match read_value(argv, "--auth-token") {
        Some(value) => value.to_owned(),
        None => match env
            .string("PILOTDECK_SDK_AUTH_TOKEN")
            .or_else(|| env.string("PILOTDECK_TOKEN"))
        {
            Some(value) => value,
            None => {
                let path = paths::pilot_home(env, cwd)?.join("server-token");
                std::fs::read(path)
                    .map(|bytes| paths::js_trim(&String::from_utf8_lossy(&bytes)).to_owned())
                    .unwrap_or_default()
            }
        },
    };
    if token.is_empty() {
        return Err(Failure::Error(
            "Gateway auth token is required. Set PILOTDECK_TOKEN or pass --auth-token.".into(),
        ));
    }
    Ok(())
}

fn positional(argv: &[String]) -> Vec<&str> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let value = argv[index].as_str();
        if VALUE_FLAGS.contains(&value) {
            index += 2;
            continue;
        }
        if !value.starts_with("--") {
            values.push(value);
        }
        index += 1;
    }
    values
}

fn read_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let index = argv.iter().position(|arg| arg == flag)?;
    argv.get(index + 1)
        .map(String::as_str)
        .filter(|value| !value.is_empty() && !value.starts_with("--"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    fn error_of(result: Result<u8, Failure>) -> String {
        match result {
            Err(Failure::Error(message)) => message,
            Err(Failure::Todo(what)) => format!("TODO {what}"),
            Ok(code) => format!("exit {code}"),
        }
    }

    #[test]
    fn help_matches_registered_bytes() {
        let expected = include_str!("../../../scripts/cli-parity/help.txt");
        assert_eq!(format!("{HELP}\n"), expected);
    }

    #[test]
    fn read_value_rejects_missing_empty_and_flag_values() {
        assert_eq!(read_value(&args(&["--auth-token"]), "--auth-token"), None);
        assert_eq!(
            read_value(&args(&["--auth-token", ""]), "--auth-token"),
            None
        );
        assert_eq!(
            read_value(&args(&["--auth-token", "--json"]), "--auth-token"),
            None
        );
        assert_eq!(
            read_value(&args(&["--auth-token", "t"]), "--auth-token"),
            Some("t")
        );
    }

    #[test]
    fn positional_skips_value_flags_and_long_options() {
        let argv = args(&["--project", "p", "{", "--json", "x", "-y"]);
        assert_eq!(positional(&argv), vec!["{", "x", "-y"]);
    }

    #[test]
    fn missing_token_falls_back_to_file_then_errors() {
        let env = Env::from_pairs(&[("PILOT_HOME", "/nonexistent-pdrs-test")]);
        let message = error_of(run_sdk_cli(
            &args(&["run", "--auth-token"]),
            &env,
            Path::new("/"),
        ));
        assert_eq!(
            message,
            "Gateway auth token is required. Set PILOTDECK_TOKEN or pass --auth-token."
        );
        // An empty env token is not nullish and is not replaced by later sources.
        let env = Env::from_pairs(&[("PILOTDECK_SDK_AUTH_TOKEN", ""), ("PILOTDECK_TOKEN", "t")]);
        let message = error_of(run_sdk_cli(
            &args(&["settings", "get"]),
            &env,
            Path::new("/"),
        ));
        assert!(message.starts_with("Gateway auth token is required."));
    }

    #[test]
    fn settings_set_checks_in_ts_order() {
        let env = Env::from_pairs(&[("PILOTDECK_SDK_AUTH_TOKEN", "t")]);
        let cwd = Path::new("/");
        assert_eq!(
            error_of(run_sdk_cli(
                &args(&["settings", "set", "--json"]),
                &env,
                cwd
            )),
            "Usage: pilotdeck settings set '<json>'"
        );
        assert_eq!(
            error_of(run_sdk_cli(&args(&["settings", "set", "{"]), &env, cwd)),
            "Invalid settings JSON: Expected property name or '}' in JSON at position 1 (line 1 column 2)"
        );
        assert_eq!(
            error_of(run_sdk_cli(&args(&["settings", "nope"]), &env, cwd)),
            "Usage: pilotdeck settings <get|set> ..."
        );
    }
}
