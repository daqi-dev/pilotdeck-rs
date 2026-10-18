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
        Some("run") => {
            create_client(&read_connection(&argv[1..], env, cwd)?)?;
            Err(Failure::Todo("pilotdeck run (SDK sessions.create)"))
        }
        Some("resume") => run_resume(&argv[1..], env, cwd),
        Some("sessions") => run_sessions(&argv[1..], env, cwd),
        Some("settings") => run_settings(&argv[1..], env, cwd),
        Some(_) => Err(Failure::Error(
            "Usage: pilotdeck <run|resume|sessions|settings> ...".into(),
        )),
    }
}

/// runTurn("resume") (sdkCli.ts:43-56): only a missing session id fails before the
/// Gateway; the prompt check (:58-59) runs after sessions.resume.
fn run_resume(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    create_client(&read_connection(argv, env, cwd)?)?;
    if positional(argv).first().is_none_or(|id| id.is_empty()) {
        return Err(Failure::Error(
            "Usage: pilotdeck resume <session-id> <prompt>".into(),
        ));
    }
    Err(Failure::Todo("pilotdeck resume (SDK sessions.resume)"))
}

/// runSessionsCommand (sdkCli.ts:135-155): the client is created, not connected,
/// before the subcommand is checked.
fn run_sessions(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    create_client(&read_connection(argv, env, cwd)?)?;
    match argv.first().map_or("list", String::as_str) {
        "list" => Err(Failure::Todo("pilotdeck sessions list (SDK sessions.list)")),
        "messages" => {
            if positional(&argv[1..])
                .first()
                .is_none_or(|id| id.is_empty())
            {
                return Err(Failure::Error(
                    "Usage: pilotdeck sessions messages <session-id>".into(),
                ));
            }
            Err(Failure::Todo(
                "pilotdeck sessions messages (SDK sessions.messages)",
            ))
        }
        _ => Err(Failure::Error(
            "Usage: pilotdeck sessions <list|messages> ...".into(),
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

struct Connection {
    gateway_url: String,
}

/// readConnection (sdkCli.ts:180-191). Only the auth token is checked here.
fn read_connection(argv: &[String], env: &Env, cwd: &Path) -> Result<Connection, Failure> {
    let gateway_url = match read_value(argv, "--gateway-url") {
        Some(value) => value.to_owned(),
        None => env
            .string("PILOTDECK_SDK_GATEWAY_URL")
            .or_else(|| env.string("PILOTDECK_GATEWAY_URL"))
            .unwrap_or_else(|| {
                let port = env
                    .string("PILOTDECK_GATEWAY_PORT")
                    .unwrap_or_else(|| "18789".into());
                format!("ws://127.0.0.1:{port}/ws")
            }),
    };
    let token = match read_value(argv, "--auth-token") {
        Some(value) => value.to_owned(),
        None => match env
            .string("PILOTDECK_SDK_AUTH_TOKEN")
            .or_else(|| env.string("PILOTDECK_TOKEN"))
        {
            Some(value) => value,
            // readFile(...).catch(() => ""): every read error, EISDIR/EACCES included,
            // is an empty token rather than a thrown error.
            None => std::fs::read(paths::gateway_token_path(env, cwd)?)
                .map(|bytes| paths::js_trim(&String::from_utf8_lossy(&bytes)).to_owned())
                .unwrap_or_default(),
        },
    };
    if token.is_empty() {
        return Err(Failure::Error(
            "Gateway auth token is required. Set PILOTDECK_TOKEN or pass --auth-token.".into(),
        ));
    }
    Ok(Connection { gateway_url })
}

/// createPilotDeckClient validates options (packages/sdk/src/client.ts:130-135) and
/// builds the transport without connecting; the token is already non-empty here.
fn create_client(connection: &Connection) -> Result<(), Failure> {
    if connection.gateway_url.is_empty() {
        return Err(Failure::Error(
            "gatewayUrl and authToken are required; pass them in query options or createPilotDeckClient defaults.".into(),
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
    fn unreadable_token_file_is_an_empty_token() {
        let home = std::env::temp_dir().join(format!("pdrs-cli-token-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("server-token")).unwrap();
        let env = Env::from_pairs(&[("PILOT_HOME", home.to_str().unwrap())]);
        let message = error_of(run_sdk_cli(
            &args(&["sessions", "bogus"]),
            &env,
            Path::new("/"),
        ));
        assert!(message.starts_with("Gateway auth token is required."));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn sessions_and_resume_usage_precede_the_gateway() {
        let env = Env::from_pairs(&[("PILOTDECK_SDK_AUTH_TOKEN", "t")]);
        let cwd = Path::new("/");
        let run = |values: &[&str]| error_of(run_sdk_cli(&args(values), &env, cwd));
        let sessions_usage = "Usage: pilotdeck sessions <list|messages> ...";
        assert_eq!(run(&["sessions", "bogus"]), sessions_usage);
        assert_eq!(run(&["sessions", ""]), sessions_usage);
        assert_eq!(run(&["sessions", "--json"]), sessions_usage);
        assert!(run(&["sessions"]).starts_with("TODO pilotdeck sessions list"));
        let messages_usage = "Usage: pilotdeck sessions messages <session-id>";
        assert_eq!(run(&["sessions", "messages"]), messages_usage);
        assert_eq!(
            run(&["sessions", "messages", "--project", "p"]),
            messages_usage
        );
        assert_eq!(run(&["sessions", "messages", ""]), messages_usage);
        assert!(run(&["sessions", "messages", "s1"]).starts_with("TODO"));
        let resume_usage = "Usage: pilotdeck resume <session-id> <prompt>";
        assert_eq!(run(&["resume"]), resume_usage);
        assert_eq!(run(&["resume", "--model", "m", "--json"]), resume_usage);
        assert_eq!(run(&["resume", ""]), resume_usage);
        // The prompt is checked only after sessions.resume reaches the Gateway.
        assert!(run(&["resume", "s1"]).starts_with("TODO pilotdeck resume"));
        assert!(run(&["run"]).starts_with("TODO pilotdeck run"));
    }

    #[test]
    fn empty_gateway_url_fails_client_creation_before_usage() {
        let url_error = "gatewayUrl and authToken are required; pass them in query options or createPilotDeckClient defaults.";
        let cwd = Path::new("/");
        let env = Env::from_pairs(&[
            ("PILOTDECK_SDK_AUTH_TOKEN", "t"),
            ("PILOTDECK_SDK_GATEWAY_URL", ""),
            ("PILOTDECK_GATEWAY_URL", "ws://x/ws"),
        ]);
        for values in [
            &["sessions", "bogus"][..],
            &["resume"][..],
            &["run", "hi"][..],
        ] {
            assert_eq!(error_of(run_sdk_cli(&args(values), &env, cwd)), url_error);
        }
        // settings never builds a client; an empty --gateway-url value is ignored.
        assert_eq!(
            error_of(run_sdk_cli(&args(&["settings", "nope"]), &env, cwd)),
            "Usage: pilotdeck settings <get|set> ..."
        );
        let env = Env::from_pairs(&[
            ("PILOTDECK_SDK_AUTH_TOKEN", "t"),
            ("PILOTDECK_GATEWAY_URL", ""),
        ]);
        assert_eq!(
            error_of(run_sdk_cli(&args(&["resume"]), &env, cwd)),
            url_error
        );
        let env = Env::from_pairs(&[
            ("PILOTDECK_SDK_AUTH_TOKEN", "t"),
            ("PILOTDECK_GATEWAY_PORT", ""),
        ]);
        assert_eq!(
            error_of(run_sdk_cli(
                &args(&["resume", "--gateway-url", ""]),
                &env,
                cwd
            )),
            "Usage: pilotdeck resume <session-id> <prompt>"
        );
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
