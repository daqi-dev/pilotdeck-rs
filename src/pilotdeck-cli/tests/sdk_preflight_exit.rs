//! Process-level preflight parity for the SDK CLI (#982, #123 D1-P01).
//!
//! Every expectation is a TS raw original from the #123 D1 matrix
//! (`_tmp/20261028-issue-123-d1/plan/raw/host-b*/cases/<grid>/`, Node v22.22.0,
//! coordinator host runs) after removing the registered Node SQLite
//! ExperimentalWarning (`scripts/cli-parity/compare.py:29-30`). Grid ids in
//! assertion messages name the original each row pins. Network outcomes are
//! P02+ and stay TODO placeholders; only the locally observable preflight
//! behavior is asserted here.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// `scripts/cli-parity/help.txt`: byte-identical to the TS help stdout in the
/// raw originals (help/help-long/help-short × every axis, 755 bytes).
const HELP: &str = include_str!("../../../scripts/cli-parity/help.txt");
const TOKEN_ERROR: &str =
    "Gateway auth token is required. Set PILOTDECK_TOKEN or pass --auth-token.\n";
const URL_ERROR: &str = "gatewayUrl and authToken are required; pass them in query options or createPilotDeckClient defaults.\n";
const SETTINGS_USAGE: &str = "Usage: pilotdeck settings <get|set> ...\n";
const SESSIONS_USAGE: &str = "Usage: pilotdeck sessions <list|messages> ...\n";
const RESUME_USAGE: &str = "Usage: pilotdeck resume <session-id> <prompt>\n";
const MESSAGES_USAGE: &str = "Usage: pilotdeck sessions messages <session-id>\n";
const SETTINGS_JSON_FLAG_ERROR: &str = "Invalid settings JSON: Unexpected non-whitespace character after JSON at position 21 (line 1 column 22)\n";
const TODO_PREFIX: &str = "pilotdeck-rs TODO: ";

fn fixture(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("pdrs-982-preflight-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("project")).expect("#982 fixture project dir");
    fs::create_dir_all(root.join("home")).expect("#982 fixture home dir");
    fs::create_dir_all(root.join("pilot")).expect("#982 fixture pilot dir");
    root
}

/// The matrix fixture shape: isolated HOME/PILOT_HOME, mock URL defaults, and
/// per-case env overrides (an empty string must stay present-but-empty).
fn spawn(root: &Path, argv: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
        .current_dir(root.join("project"))
        .env_clear()
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("CFFIXED_USER_HOME", root.join("home"))
        .env("PILOT_HOME", root.join("pilot"))
        .env("PILOTDECK_CONFIG_PATH", root.join("pilot/pilotdeck.yaml"))
        .env("TMPDIR", root.join("home"))
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("PILOTDECK_GATEWAY_PORT", "28999")
        .env("PILOTDECK_GATEWAY_HOST", "127.0.0.1")
        .env("PILOTDECK_GATEWAY_URL", "ws://127.0.0.1:28999/ws")
        .env("PILOTDECK_SDK_GATEWAY_URL", "ws://127.0.0.1:28999/ws")
        .envs(env.iter().map(|(k, v)| (k.to_string(), v.to_string())))
        .args(argv)
        .output()
        .expect("#982 spawn pilotdeck")
}

fn assert_case(
    root: &Path,
    argv: &[&str],
    env: &[(&str, &str)],
    exit: i32,
    stdout: &str,
    stderr: &str,
    grid: &str,
) {
    let output = spawn(root, argv, env);
    assert_eq!(
        output.status.code(),
        Some(exit),
        "#982 {grid}: exit code, stderr was {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        stdout,
        "#982 {grid}: stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        stderr,
        "#982 {grid}: stderr"
    );
}

#[test]
fn help_aliases_and_flags_match_ts_bytes() {
    let root = fixture("help");
    for (grid, argv) in [
        ("help--argv", vec!["help"]),
        ("help-long--argv", vec!["--help"]),
        ("help-short--argv", vec!["-h"]),
        (
            "help--token-argv",
            vec!["help", "--auth-token", "d1-fixture-not-a-secret"],
        ),
        ("help--missing-project", vec!["help", "--project"]),
        ("help--missing-auth-token", vec!["help", "--auth-token"]),
        ("help--format-both", vec!["help", "--json", "--stream-json"]),
        (
            "help--flag-unknown",
            vec!["help", "--unknown", "tail", "-x"],
        ),
        ("help--flag-equals", vec!["help", "--project=/d1-project"]),
        (
            "help--flag-repeat",
            vec!["help", "--project", "/first", "--project", "/second"],
        ),
        (
            "help--flag-combined",
            vec![
                "help",
                "--project",
                "/d1-project",
                "--channel",
                "fixture-channel",
                "--permission-mode",
                "dontAsk",
            ],
        ),
        (
            "help--gateway-argv-precedence",
            vec!["help", "--gateway-url", "ws://127.0.0.1:28999/ws"],
        ),
    ] {
        assert_case(&root, &argv, &[], 0, HELP, "", grid);
    }

    // help--stdin-argv-priority: stdin is never a prompt source for help.
    let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
        .current_dir(root.join("project"))
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PILOT_HOME", root.join("pilot"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("help")
        .spawn()
        .expect("#982 spawn pilotdeck with stdin");
    // help never reads stdin and may exit before the write lands, so a broken
    // pipe is the expected outcome rather than a failure (#1007).
    match child
        .stdin
        .take()
        .expect("#982 stdin pipe")
        .write_all(b"must not replace argv\n")
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(error) => panic!("#982 stdin write: {error:?}"),
    }
    let output = child.wait_with_output().expect("#982 help stdin output");
    assert_eq!(
        output.status.code(),
        Some(0),
        "#982 help--stdin-argv-priority: exit code"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        HELP,
        "#982 help--stdin-argv-priority: stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "#982 help--stdin-argv-priority: stderr"
    );
    fs::remove_dir_all(root).expect("#982 remove help fixture");
}

#[test]
fn token_source_errors_match_ts_bytes() {
    let root = fixture("token");
    // token-absent: no env token, no token file.
    for (grid, argv) in [
        ("run--token-absent", vec!["run", "fixture prompt"]),
        (
            "resume--token-absent",
            vec!["resume", "d1-session", "fixture prompt"],
        ),
        ("sessions-list--token-absent", vec!["sessions", "list"]),
        ("settings-get--token-absent", vec!["settings", "get"]),
    ] {
        assert_case(&root, &argv, &[], 1, "", TOKEN_ERROR, grid);
    }
    // token-sdk-empty: a present-but-empty SDK token is not nullish in TS, so
    // the generic env token and the token file must not replace it.
    for (grid, argv) in [
        ("run--token-sdk-empty", vec!["run", "fixture prompt"]),
        (
            "resume--token-sdk-empty",
            vec!["resume", "d1-session", "fixture prompt"],
        ),
        ("sessions-list--token-sdk-empty", vec!["sessions", "list"]),
        (
            "settings-set--token-sdk-empty",
            vec!["settings", "set", "{\"language\":\"zh-CN\"}"],
        ),
    ] {
        assert_case(
            &root,
            &argv,
            &[
                ("PILOTDECK_SDK_AUTH_TOKEN", ""),
                ("PILOTDECK_TOKEN", "d1-fixture-not-a-secret"),
            ],
            1,
            "",
            TOKEN_ERROR,
            grid,
        );
    }

    // run--token-file: the file source satisfies preflight, so the run leaves
    // the local path and stops at the P02 TODO placeholder (exit 2).
    fs::write(
        root.join("pilot/server-token"),
        " \nd1-fixture-not-a-secret\t\n",
    )
    .expect("#982 write token file");
    let output = spawn(&root, &["run", "fixture prompt"], &[]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "#982 run--token-file: preflight passes, P02 TODO exit"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("{TODO_PREFIX}pilotdeck run (SDK sessions.create) is not implemented yet.\n"),
        "#982 run--token-file: TODO stderr"
    );
    fs::remove_dir_all(root).expect("#982 remove token fixture");
}

#[test]
fn gateway_empty_fails_the_shared_gate_before_connecting() {
    let root = fixture("url");
    for (grid, argv) in [
        ("run--gateway-empty", vec!["run", "fixture prompt"]),
        (
            "resume--gateway-empty",
            vec!["resume", "d1-session", "fixture prompt"],
        ),
        ("sessions-default--gateway-empty", vec!["sessions"]),
        ("sessions-list--gateway-empty", vec!["sessions", "list"]),
        (
            "sessions-messages--gateway-empty",
            vec!["sessions", "messages", "d1-session"],
        ),
        ("settings-default--gateway-empty", vec!["settings"]),
        ("settings-get--gateway-empty", vec!["settings", "get"]),
        (
            "settings-set--gateway-empty",
            vec!["settings", "set", "{\"language\":\"zh-CN\"}"],
        ),
    ] {
        assert_case(
            &root,
            &argv,
            &[
                ("PILOTDECK_SDK_AUTH_TOKEN", "d1-fixture-not-a-secret"),
                ("PILOTDECK_SDK_GATEWAY_URL", ""),
            ],
            1,
            "",
            URL_ERROR,
            grid,
        );
    }
    // settings-unknown--gateway-empty: the subcommand dispatch precedes the
    // connectionOptions gate, so the usage error still wins under an empty URL.
    assert_case(
        &root,
        &["settings", "bogus"],
        &[
            ("PILOTDECK_SDK_AUTH_TOKEN", "d1-fixture-not-a-secret"),
            ("PILOTDECK_SDK_GATEWAY_URL", ""),
        ],
        1,
        "",
        SETTINGS_USAGE,
        "settings-unknown--gateway-empty",
    );
    fs::remove_dir_all(root).expect("#982 remove url fixture");
}

#[test]
fn usage_and_json_errors_match_ts_bytes() {
    let root = fixture("usage");
    let token = [("PILOTDECK_SDK_AUTH_TOKEN", "d1-fixture-not-a-secret")];
    assert_case(
        &root,
        &["resume"],
        &token,
        1,
        "",
        RESUME_USAGE,
        "resume--missing-session",
    );
    assert_case(
        &root,
        &["sessions", "messages"],
        &token,
        1,
        "",
        MESSAGES_USAGE,
        "sessions-messages--missing-session",
    );
    assert_case(
        &root,
        &["sessions", "--unknown", "tail", "-x"],
        &token,
        1,
        "",
        SESSIONS_USAGE,
        "sessions-default--flag-unknown",
    );
    // settings-set--flag-unknown / --missing-h: the trailing positionals join
    // the value, so the JSON syntax error fires before the URL gate.
    for (grid, argv) in [
        (
            "settings-set--flag-unknown",
            vec![
                "settings",
                "set",
                "{\"language\":\"zh-CN\"}",
                "--unknown",
                "tail",
                "-x",
            ],
        ),
        (
            "settings-set--missing-h",
            vec!["settings", "set", "{\"language\":\"zh-CN\"}", "-h"],
        ),
    ] {
        assert_case(&root, &argv, &token, 1, "", SETTINGS_JSON_FLAG_ERROR, grid);
    }
    assert_case(
        &root,
        &["settings", "nope"],
        &token,
        1,
        "",
        SETTINGS_USAGE,
        "settings-unknown--argv",
    );
    fs::remove_dir_all(root).expect("#982 remove usage fixture");
}
