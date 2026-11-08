//! #1353 A8.5-pr2 A13: the switch machine behind the real `--native` entry.
//! SIGTERM starts the matrix §二 sequence on an idle server — no clients, no
//! `cron:` section — and the completed switch is the process's own clean
//! exit. Every way the wiring can break has its own observation here: a
//! signal path that never reaches the machine leaves the default SIGTERM
//! disposition in place, so the child dies as a signal death (a failed
//! status, not exit 0); a halting machine prints "native switch halted" and
//! exits nonzero; a machine that hangs is bounded by its own face budgets
//! (the `--drain-ms` spelling below) and still exits.
use std::{path::PathBuf, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::Command,
    time::timeout,
};

// The RFC 6455 public handshake sample key, shared with the crate-side
// tests through the same common file.
#[path = "common/native_ws_sample.rs"]
mod native_ws_sample;
use native_ws_sample::RFC6455_SAMPLE_NONCE;
// The constants shared with `tests/mode_routing.rs`, the same native entry's
// sibling test (#1354): one spelling per product fact.
#[path = "common/native_entry.rs"]
mod native_entry;
use native_entry::{
    ADVERTISEMENT_BUDGET, NATIVE_LISTENING, NATIVE_STOPPING, STDERR_LIMIT, SWITCH_DEADLINE,
    VALID_HOME_CONFIG,
};

// Removed with the test's scratch tree once the child has been reaped: the
// path is unique per run and nothing else ever writes it.
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_root(tag: &str) -> (Cleanup, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "pdrs-1353-switch-e2e-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root).expect("#1353 switch e2e scratch directory must be new");
    (Cleanup(root.clone()), root)
}

#[tokio::test]
async fn sigterm_runs_the_switch_and_exits_clean() {
    let (_cleanup, root) = scratch_root("sigterm");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir(&home).expect("#1353 switch e2e home directory");
    std::fs::create_dir(&project).expect("#1353 switch e2e project directory");
    let token_file = root.join("token");
    std::fs::write(&token_file, "1353-switch-e2e-token\n").expect("#1353 switch e2e token file");
    // Never executed: the sidecar is a per-session stdio deployment and this
    // server never serves a session, but the entry requires the file to exist.
    let sidecar = root.join("sidecar");
    std::fs::write(&sidecar, "#1353 never executed by an idle server\n")
        .expect("#1353 switch e2e sidecar file");
    std::fs::write(home.join("pilotdeck.yaml"), VALID_HOME_CONFIG)
        .expect("#1353 switch e2e home config file");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .arg("--native")
        .arg("--home")
        .arg(&home)
        .arg("--project")
        .arg(&project)
        .arg("--listen")
        .arg("127.0.0.1:0")
        .arg("--token-file")
        .arg(&token_file)
        .arg("--sidecar")
        .arg(&sidecar)
        // Bounded, and the machine's entry/owner face bounds derive from it.
        .arg("--drain-ms")
        .arg("5000")
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("#1353 switch e2e child launch");
    let stderr = child
        .stderr
        .take()
        .expect("#1353 switch e2e child stderr pipe");
    let mut lines = BufReader::new(stderr).take(STDERR_LIMIT).lines();
    // Wait for the child's own listening advertisement: past this point the
    // assembly is up and the signal handlers are registered.
    let mut advertisement = String::new();
    let advertised = timeout(ADVERTISEMENT_BUDGET, async {
        while let Some(line) = lines
            .next_line()
            .await
            .expect("#1353 switch e2e stderr read")
        {
            if native_entry::advertises(&line) {
                advertisement = line;
                return true;
            }
        }
        false
    })
    .await
    .expect("#1353 switch e2e advertisement deadline");
    assert!(
        advertised,
        "#1353 switch e2e child never advertised listening (last line: {advertisement:?})"
    );
    // Steady state is actually served before the signal: a client that
    // completes the WS upgrade against the advertised address proves the
    // accept loop is live from launch — the phased run the switch later
    // drives is constructed before the signal wait, so serving must not
    // wait for SIGTERM (a regression here hangs the upgrade instead of the
    // clean exit below).
    let address = advertisement
        .strip_prefix(NATIVE_LISTENING)
        .and_then(|rest| rest.strip_prefix(' '))
        .expect("#1353 switch e2e advertised address");
    let mut client = timeout(ADVERTISEMENT_BUDGET, TcpStream::connect(address))
        .await
        .expect("#1353 switch e2e steady-state connect deadline")
        .expect("#1353 switch e2e steady-state connect");
    client
        .write_all(
            format!(
                "GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                 Upgrade: websocket\r\nSec-WebSocket-Key: {RFC6455_SAMPLE_NONCE}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("#1353 switch e2e steady-state upgrade write");
    let mut header = Vec::new();
    timeout(ADVERTISEMENT_BUDGET, async {
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            client
                .read_exact(&mut byte)
                .await
                .expect("#1353 switch e2e steady-state upgrade byte");
            header.push(byte[0]);
        }
    })
    .await
    .expect("#1353 switch e2e steady-state upgrade deadline");
    assert!(
        header.starts_with(b"HTTP/1.1 101"),
        "#1353 switch e2e steady-state upgrade status, got {}",
        String::from_utf8_lossy(&header)
    );
    // The client's whole job is the upgrade: drop it before the signal, so
    // the switch below still runs over an idle server (a still-connected
    // client would expire the drain budget and halt — a different cell).
    drop(client);
    // The signal the production entry is built around. `kill_on_drop` only
    // SIGKILLs, so the wait below observes the machine's own exit, not this.
    // rustix's safe `kill` wrapper: the workspace forbids `unsafe`, so the
    // raw `libc::kill` call is made through the same syscall's wrapper (the
    // `handle_path.rs` precedent).
    let pid =
        rustix::process::Pid::from_raw(child.id().expect("#1353 switch e2e child pid") as i32)
            .expect("#1353 switch e2e child pid in range");
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)
        .expect("#1353 switch e2e SIGTERM delivery");
    // Read to EOF (the child's exit ends the pipe) under the switch bound:
    // the tail is the evidence for which exit path ran.
    let mut tail = Vec::new();
    timeout(SWITCH_DEADLINE, async {
        while let Some(line) = lines.next_line().await.expect("#1353 switch e2e tail read") {
            tail.push(line);
        }
    })
    .await
    .expect("#1353 switch e2e switch deadline");
    let status = timeout(SWITCH_DEADLINE, child.wait())
        .await
        .expect("#1353 switch e2e child exit deadline")
        .expect("#1353 switch e2e child exit status");
    // The machine's S2 began — the signal reached the switch, not a default
    // disposition death (which would print nothing more and fail below).
    assert!(
        tail.iter().any(|line| line == NATIVE_STOPPING),
        "#1353 the child never announced the switch; tail: {tail:?}"
    );
    // An idle server has nothing to halt on: a halt here means the machine
    // or its teardown is broken, and the tail names which step.
    assert!(
        !tail
            .iter()
            .any(|line| line.starts_with("native switch halted")),
        "#1353 the switch over an idle server must complete, not halt; tail: {tail:?}"
    );
    // A13: the completed machine is the clean exit. A signal death, a halt
    // ("native switch halted" in the tail) and a teardown failure all land
    // here as a failed status with the tail as evidence.
    assert!(
        status.success(),
        "#1353 the completed switch exits 0, got {status}; tail: {tail:?}"
    );
}
