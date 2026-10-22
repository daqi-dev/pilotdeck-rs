//! The oracle comes from the unmodified TS CLI action and native JSONL provider.
use std::{fs, process::Command};

fn parity(source: &str, errors_only: bool, bounded: Option<&str>) {
    let oracle: serde_json::Value = serde_json::from_str(source).unwrap();
    assert_eq!(
        oracle["provenance"]["sourceCommit"],
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    let root = std::env::temp_dir().join(format!(
        "pdrs-chat-exit-{}-{errors_only}-{}-{}",
        std::process::id(),
        source.len(),
        bounded.unwrap_or("ordinary")
    ));
    fs::create_dir_all(root.join("project")).unwrap();
    for (name, text) in oracle["files"].as_object().unwrap() {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = text
            .as_str()
            .unwrap()
            .replace("{runtime}", &root.to_string_lossy());
        if bounded == Some("bounded-literal") && name.ends_with("/literal.jsonl") {
            // Repeat nonmatching TS input, preserving the exact TS output/query.
            // The 32 rows force the old sliding window past the CPU budget.
            text = (text + "\n").repeat(32);
        }
        fs::write(path, text).unwrap();
    }
    let mut failures = Vec::new();
    let mut checked = 0;
    for case in oracle["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let pre_storage = matches!(id, "usage" | "unknown" | "empty" | "flags-only");
        if pre_storage != errors_only
            || match bounded {
                Some(prefix) => !id.starts_with(prefix),
                None => id.starts_with("bounded-"),
            }
        {
            continue;
        }
        checked += 1;
        let args: Vec<_> = case["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let mut command = search_command(bounded.is_some());
        command
            .arg("chat")
            .args(args)
            .env_clear()
            .env("HOME", root.join("home"))
            .env("PILOT_HOME", root.join("pilot"))
            .env("TZ", case["tz"].as_str().unwrap_or("UTC"))
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("NO_COLOR", "1")
            .current_dir(root.join("project"));
        let output = if bounded.is_some() {
            bounded_output(&mut command, id)
        } else {
            command.output().unwrap()
        };
        let expected = |key: &str| {
            case[key]
                .as_str()
                .unwrap()
                .replace("{runtime}", &root.to_string_lossy())
                .into_bytes()
        };
        if output.status.code() != case["exit"].as_i64().map(|n| n as i32)
            || output.stdout != expected("stdout")
            || output.stderr != expected("stderr")
        {
            failures.push(format!(
                "{id}: exit={:?} stdout={:?} stderr={:?}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    fs::remove_dir_all(&root).unwrap();
    assert!(
        checked > 0,
        "case selection must not silently run zero cases"
    );
    assert!(
        failures.is_empty(),
        "TS CLI parity mismatches:\n{}",
        failures.join("\n")
    );
}

// RLIMIT_CPU counts CPU consumed by this process, not time waiting to be
// scheduled. /bin/sh sets both limits then execs the CLI in the same PID, so
// the watchdog can kill/reap the actual CLI without leaving a wrapper's child.
// No shell interpolation: all CLI arguments are passed separately via "$@".
fn search_command(bounded: bool) -> Command {
    if !bounded {
        return Command::new(env!("CARGO_BIN_EXE_pilotdeck"));
    }
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "ulimit -t 4 || exit 125; exec \"$@\"",
            "chat-search-cpu-budget",
        ])
        .arg(env!("CARGO_BIN_EXE_pilotdeck"));
    command
}

// Drain both pipes while polling: a large JSON query must not block on a full
// stdout pipe. CPU exhaustion is a nonzero exit; 120 s is only a hang watchdog.
// macOS debug measurements (fix6-*.log in RESULT): the old class materializer
// on the unchanged 65,000-escape input used 83.63 CPU s / 534 MiB before being
// killed at 120 s. Original literal windows used 0.29-0.33 CPU s per row;
// at 32 rows it used 7.05/5.94 CPU s, versus KMP's 1.06/0.24 CPU s for the
// two case modes. A 4 CPU s limit is insensitive to scheduler load;
// the wall watchdog has >50x the measured 2.11 s new literal wall time.
fn bounded_output(command: &mut Command, id: &str) -> std::process::Output {
    use std::{
        io::Read,
        os::unix::process::ExitStatusExt,
        process::Stdio,
        time::{Duration, Instant},
    };
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = drain(Box::new(child.stdout.take().unwrap()));
    let stderr = drain(Box::new(child.stderr.take().unwrap()));
    let mut killed_at_deadline = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(120) {
            // Recheck at the deadline. If exit races with kill, wait's actual
            // status wins: a successful exit must not become a timeout merely
            // because this polling thread or the pipe drain ran late.
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            // kill may lose the race to a normal exit; still reap and drain.
            let _ = child.kill();
            let status = child.wait().unwrap();
            killed_at_deadline = status.signal() == Some(libc::SIGKILL);
            break status;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let output = std::process::Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert!(
        !killed_at_deadline,
        "{id}: 120 s hang watchdog killed and reaped child; elapsed={:?}, stdout={} bytes, stderr={} bytes",
        started.elapsed(),
        output.stdout.len(),
        output.stderr.len(),
    );
    assert!(
        output.status.success(),
        "{id}: bounded child failed (4 CPU s budget); status={}, elapsed={:?}, stdout={} bytes, stderr={} bytes; stderr={:?}",
        output.status,
        started.elapsed(),
        output.stdout.len(),
        output.stderr.len(),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

#[test]
fn chat_search_large_character_classes_are_bounded() {
    parity(
        include_str!("fixtures/chat-search/edges.json"),
        false,
        Some("bounded-class"),
    );
}

#[test]
fn chat_search_large_literal_prefixes_are_bounded() {
    parity(
        include_str!("fixtures/chat-search/edges.json"),
        false,
        Some("bounded-literal"),
    );
}

#[test]
fn chat_search_usage_and_keyword_errors_match_ts() {
    parity(include_str!("fixtures/chat-search/oracle.json"), true, None);
}

#[test]
fn chat_search_storage_and_format_match_ts() {
    parity(
        include_str!("fixtures/chat-search/oracle.json"),
        false,
        None,
    );
}

#[test]
fn chat_search_utf16_and_raw_storage_edges_match_ts() {
    parity(include_str!("fixtures/chat-search/edges.json"), false, None);
}

#[test]
fn chat_search_backtracking_limit_is_a_bounded_error() {
    let root = std::env::temp_dir().join(format!("pdrs-chat-backtrack-{}", std::process::id()));
    let chats = root.join("pilot/projects/backtrack/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join("s.jsonl"),
        serde_json::to_vec(&serde_json::json!({
            "type":"accepted_input", "sessionId":"s", "messages":[{
                "content":[{"type":"text", "text":format!("{}!", "a".repeat(100))}]
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
        .args([
            "chat",
            "search",
            r"\b(a+)+$",
            "--regex",
            "--all-projects",
            "--json",
        ])
        .env_clear()
        .env("HOME", &root)
        .env("PILOT_HOME", root.join("pilot"))
        .current_dir(&root)
        .output()
        .unwrap();
    fs::remove_dir_all(&root).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"Chat search regex exceeded backtracking limit\n"
    );
}
