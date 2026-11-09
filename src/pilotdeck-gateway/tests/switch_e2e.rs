//! #1353 A8.5-pr2 A13: the switch machine behind the real `--native` entry.
//! SIGTERM starts the matrix §二 sequence on an idle server — no clients, no
//! `cron:` section — and the completed switch is the process's own clean
//! exit. Every way the wiring can break has its own observation here: a
//! signal path that never reaches the machine leaves the default SIGTERM
//! disposition in place, so the child dies as a signal death (a failed
//! status, not exit 0); a halting machine prints the shared
//! `NATIVE_SWITCH_HALTED` line and exits nonzero; a machine that hangs is
//! bounded by its own face budgets (the `--drain-ms` spelling below) and
//! still exits.
use std::{path::PathBuf, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    time::timeout,
};

// The constants and stop-side timing segments shared with
// `tests/mode_routing.rs`, the same native entry's sibling test (#1354 /
// #2511): one spelling per product fact, one copy per shared segment.
#[path = "common/native_entry.rs"]
mod native_entry;
use native_entry::{ADVERTISEMENT_BUDGET, STDERR_LIMIT, SWITCH_DEADLINE, VALID_HOME_CONFIG};

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
    // This consumer's spelling of the shared `STDERR_LIMIT` (#2511 item 2):
    // `take` bounds the read itself — the sibling in `tests/mode_routing.rs`
    // keeps draining and bounds only retention instead.
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
    // clean exit below). The probe itself is the shared segment (#2511
    // item 1); it drops the client on the way out, so the switch below
    // still runs over an idle server (a still-connected client would expire
    // the drain budget and halt — a different cell).
    let address = native_entry::advertised_address(&advertisement)
        .expect("#1353 switch e2e advertised address");
    native_entry::steady_state_ws_upgrade(address).await;
    // The signal the production entry is built around (the shared handoff,
    // #2511 item 1): `kill_on_drop` only SIGKILLs, so the wait below
    // observes the machine's own exit, not this.
    native_entry::sigterm_child(&mut child).await;
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
    // The machine's S2 began, never halted, and the completed machine is the
    // clean exit — the shared verdicts, with the tail asserted non-empty
    // first so the "no halt line" judgement cannot pass over zero output
    // (#2511 item 1). A signal death, a halt and a teardown failure all land
    // there as a failed status with the tail as evidence.
    native_entry::assert_completed_switch(&tail, &status);
}
