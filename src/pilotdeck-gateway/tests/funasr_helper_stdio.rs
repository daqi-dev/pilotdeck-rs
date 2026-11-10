//! Real helper process: empty install command and non-UTF-8 inherited environment.
#![cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

#[tokio::test]
async fn empty_install_command_with_non_utf8_environment_still_serves() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .args(["--builtin-mcp", "funasr", "--install-command", ""])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PILOTDECK_FUNASR_INSTALL_COMMAND", "fixture install")
        .env("FIXTURE_NON_UTF8", OsString::from_vec(vec![0xff, b'x']))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn actual gateway helper");
    let mut stdin = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"transcribe_audio\",\"arguments\":{}}}\n").await.unwrap();
    let mut line = String::new();
    timeout(Duration::from_secs(10), output.read_line(&mut line))
        .await
        .expect("helper response deadline")
        .expect("read helper");
    let body: serde_json::Value = serde_json::from_str(&line).expect("one JSON-RPC response");
    assert_eq!(body["id"], 1);
    assert_eq!(body["result"]["isError"], true);
    assert!(
        body["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Run fixture install"),
        "{body}"
    );
    drop(stdin);
    let status = timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("helper exits after EOF")
        .expect("wait helper");
    assert!(status.success(), "{status}");
}
