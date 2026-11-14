//! #1354 A8.5-pr3: the main entry's default routing switch, and the proxy
//! startup mode kept as the isolated rollback target.
//!
//! Coverage matrix (mode × entry × switch direction), every cell on the real
//! executable:
//!
//! | routing | entry | observation |
//! |---|---|---|
//! | proxy (default) | nothing set | the two A4 address lines, TCP reachable |
//! | proxy (explicit) | `PILOTDECK_GATEWAY_MODE=proxy` | the same |
//! | native (explicit) | `PILOTDECK_GATEWAY_MODE=native` | `native listening` + WS 101, SIGTERM exits 0 |
//! | native (explicit) | `--native` first argument | the parser selection beats a `proxy` config item |
//! | rejected | any other value (incl. empty, non-UTF-8) | nonzero exit with the explicit gate |
//!
//! The drill test then alternates both modes on one isolated data directory
//! (scratch home, `127.0.0.1:0` everywhere): proxy → native → proxy (the
//! rollback) → native again. Deep data compatibility (Rust writes read back
//! by TS, rollback not clobbering newer data) is #1356's drill, not this
//! one; here the process-level alternation over the same directory is the
//! delivered rollback rehearsal. The exact bytes of the A4 address lines are
//! pinned by `tests/proxy_listen_address.rs` against its TS oracle; this
//! file asserts their shape and reachability, not a second byte copy.
use std::{
    ffi::OsString,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};
use tokio::{io::AsyncBufReadExt, process::Command as AsyncCommand, time::timeout};

// The constants and stop-side timing segments shared with
// `tests/switch_e2e.rs`, the same native entry's sibling test: one spelling
// per product fact, one copy per shared segment (#2511 item 1).
#[path = "common/native_entry.rs"]
mod native_entry;
use native_entry::{
    ADVERTISEMENT_BUDGET, STDERR_LIMIT, SWITCH_DEADLINE, VALID_HOME_CONFIG, advertised_address,
};

// The product's own advertisement lines, exactly as it writes them: the A4
// proxy on stdout (`println!` in src/main.rs) and the native entry on stderr
// (`console_log_line` in src/native_process.rs).
const PROXY_HTTP_PREFIX: &str = "PilotDeck server listening: http://";
const PROXY_WS_PREFIX: &str = "WebSocket: ws://";
// A rejected child exits by itself right after printing the rejection, so
// this only bounds a child that hangs on its way out.
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_root(tag: &str) -> (Cleanup, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "pdrs-1354-mode-routing-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    // #3143 fix-r2 (DS sibling#5, #2335): the guard exists before the first
    // fallible disk op -- a panic below otherwise leaks the scratch root,
    // because only a constructed `Cleanup` unwinds through `Drop`.
    let mut cleanup = Cleanup(root);
    std::fs::create_dir(&cleanup.0).expect("#1354 mode routing scratch directory must be new");
    // The native child canonicalizes its `--home`/`--project` (`Options::parse`
    // judges the canonical path); a symlinked temp root (macOS `/var` →
    // `/private/var`) would split the test's raw spelling from the child's,
    // so every path below starts from the canonical root.
    cleanup.0 = cleanup
        .0
        .canonicalize()
        .expect("#1354 canonical scratch root");
    let root = cleanup.0.clone();
    (cleanup, root)
}

/// The native entry's explicit directories and files (the switch_e2e
/// fixtures): every path absolute, under the run's scratch root only.
fn native_fixtures(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir(&home).expect("#1354 native home directory");
    std::fs::create_dir(&project).expect("#1354 native project directory");
    std::fs::write(home.join("pilotdeck.yaml"), VALID_HOME_CONFIG)
        .expect("#1354 native home config");
    std::fs::write(root.join("token"), "1354-mode-routing-token\n")
        .expect("#1354 native token file");
    // Never executed: the sidecar is a per-session stdio deployment and this
    // server never serves a session, but the entry requires the file to exist.
    std::fs::write(
        root.join("sidecar"),
        "#1354 never executed by an idle server\n",
    )
    .expect("#1354 native sidecar file");
    (home, project)
}

// One proxy-mode child: `env_clear`, the isolated home as both `HOME` and
// cwd, an ephemeral listen address and an unreachable upstream — the A4
// front serves its listener regardless of upstream reachability
// (`tests/proxy_listen_address.rs` precedent).
struct ProxyChild {
    child: Child,
    lines: std::sync::mpsc::Receiver<String>,
    reader: Option<thread::JoinHandle<()>>,
}

impl ProxyChild {
    fn start(home: &std::path::Path, mode: Option<&str>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"));
        command
            .env_clear()
            .env("HOME", home)
            .env("PILOTDECK_PROXY_LISTEN", "127.0.0.1:0")
            .env("PILOTDECK_PROXY_UPSTREAM", "http://127.0.0.1:1")
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(mode) = mode {
            command.env("PILOTDECK_GATEWAY_MODE", mode);
        }
        let mut child = command.spawn().expect("#1354 proxy child launch");
        let stdout = child.stdout.take().expect("#1354 proxy stdout pipe");
        let (send, lines) = std::sync::mpsc::channel();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            // Retained lines are bounded (the shared `STDERR_LIMIT`
            // semantics): the test only needs the first lines, and a chatty
            // child must not grow the channel without limit while the pipe
            // still gets drained.
            let mut retained = 0u64;
            loop {
                let mut line = String::new();
                if reader
                    .read_line(&mut line)
                    .expect("#1354 proxy stdout read")
                    == 0
                {
                    break;
                }
                retained += line.len() as u64;
                if retained > STDERR_LIMIT {
                    continue;
                }
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
        }
    }

    fn line(&mut self) -> String {
        self.lines
            .recv_timeout(ADVERTISEMENT_BUDGET)
            .unwrap_or_else(|error| {
                panic!(
                    "#1354 proxy must publish its address lines: {error}; status: {:?}",
                    self.child.try_wait()
                )
            })
    }
}

impl Drop for ProxyChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Wait for the two A4 address lines and return the advertised address,
/// proving the child's routing selected the proxy front.
fn proxy_advertises_address(proxy: &mut ProxyChild) -> std::net::SocketAddr {
    let http = proxy.line();
    let address: std::net::SocketAddr = http
        .strip_prefix(PROXY_HTTP_PREFIX)
        .unwrap_or_else(|| panic!("#1354 http address line shape, got {http:?}"))
        .strip_suffix('\n')
        .expect("#1354 address line ends with newline")
        .parse()
        .expect("#1354 advertised address parses");
    let ws = proxy.line();
    assert_eq!(
        ws,
        format!("{PROXY_WS_PREFIX}{address}/ws\n"),
        "#1354 ws address line carries the same address"
    );
    std::net::TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .expect("#1354 advertised proxy address accepts TCP connections");
    address
}

/// Kill the child and report the bound as breached; both deadline branches
/// (`stderr goes quiet` and `stderr closed but the process lingers`) end here.
fn kill_at_bound(child: &mut Child, collected: &str) -> ! {
    let _ = child.kill();
    let _ = child.wait();
    panic!("#1354 rejected child did not exit within the bound; stderr so far: {collected}");
}

/// A rejected child exits by itself; read its whole stderr under a bound.
/// A child that wrongly stays alive — the exact regression this bound exists
/// for — is killed and reported with the stderr it produced, never left to
/// hang the harness (a hang reads as a timeout, not a failed assertion).
/// The deadline is checked before every receive, not only through the
/// receive's remaining slice: a chatty child keeps the queue non-empty, and a
/// zero-duration receive on a non-empty queue returns `Ok` immediately, so
/// the timeout branch alone would never fire for it.
fn collect_exit(mut child: Child) -> (std::process::ExitStatus, String) {
    let stderr = child.stderr.take().expect("#1354 stderr pipe");
    let (send, lines) = std::sync::mpsc::channel::<String>();
    let reader = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            let line = line.expect("#1354 stderr read");
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let started = std::time::Instant::now();
    let mut collected = String::new();
    loop {
        if started.elapsed() >= EXIT_DEADLINE {
            kill_at_bound(&mut child, &collected);
        }
        // Saturating, not subtracting: a clock tick past the deadline between
        // the check above and here must not panic `Duration` (a panicking
        // harness drops the child without killing it) — zero remaining falls
        // through to `Timeout`, and the loop head is the judge.
        let remaining = EXIT_DEADLINE.saturating_sub(started.elapsed());
        match lines.recv_timeout(remaining) {
            Ok(line) => {
                // Retained evidence is bounded like the read side's
                // `take(STDERR_LIMIT)`: past the limit keep draining (the
                // reader must still reach EOF) but retain no more, so a
                // chatty child cannot grow the string. The cut backs off to
                // the nearest char boundary — the limit is a byte count and
                // `String::truncate` panics mid-character, and a panicking
                // harness would drop the child without killing it.
                if collected.len() < STDERR_LIMIT as usize {
                    collected.push_str(&line);
                    collected.push('\n');
                    if collected.len() > STDERR_LIMIT as usize {
                        let mut bound = STDERR_LIMIT as usize;
                        while !collected.is_char_boundary(bound) {
                            bound -= 1;
                        }
                        collected.truncate(bound);
                    }
                }
            }
            // EOF on the pipe: the read side is done. The exit itself is
            // still bounded below — a child that closed stderr but lingers
            // must not hang the harness either.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            // The remaining slice ran out; the loop head above is the judge.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    let joined = reader.join();
    let status = loop {
        if started.elapsed() >= EXIT_DEADLINE {
            kill_at_bound(&mut child, &collected);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("#1354 child exit wait: {error}"),
        }
    };
    if let Err(panic) = joined {
        std::panic::resume_unwind(panic);
    }
    (status, collected)
}

// #1303 option A, executable level: without either explicit switch the entry
// is the A4 proxy — the default this issue must pin — and the explicit
// `proxy` value selects the same front.
#[test]
fn default_and_explicit_proxy_route_the_proxy_front() {
    for mode in [None, Some("proxy")] {
        let (_cleanup, root) = scratch_root("proxy-front");
        let home = root.join("home");
        std::fs::create_dir(&home).expect("#1354 proxy scratch home");
        let mut proxy = ProxyChild::start(&home, mode);
        let address = proxy_advertises_address(&mut proxy);
        assert_ne!(
            address.port(),
            0,
            "#1354 advertised proxy port must be nonzero"
        );
        assert_eq!(
            address.ip(),
            std::net::Ipv4Addr::LOCALHOST,
            "#1354 advertised proxy address is loopback"
        );
    }
}

// The config item's native routing: the full command line is the native
// options grammar (no `--native` flag), the child advertises `native
// listening`, serves a real WS upgrade, and the signal switch (#1353
// machine) completes as the clean exit.
#[tokio::test]
async fn mode_config_routes_the_native_entry() {
    let (_cleanup, root) = scratch_root("native-config");
    native_fixtures(&root);
    native_child_serves(&root, |command| {
        command.env("PILOTDECK_GATEWAY_MODE", "native");
    })
    .await;
}

// A rejected child exits by itself with the explicit gate; every spelling
// that is not exactly `proxy` or `native` — the empty string, case variants,
// surrounding whitespace, unrelated words, and non-UTF-8 bytes — fails
// closed at startup (#1369 shape). The listen address is deliberately
// unparseable so a child that wrongly falls through to the proxy front
// fails there immediately instead of binding any port.
#[test]
fn invalid_mode_config_fails_closed() {
    let mut values: Vec<OsString> = [
        "", " ", "PROXY", "Native", "native ", " native", "true", "1", "proxy\n",
    ]
    .iter()
    .map(|value| OsString::from(*value))
    .collect();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        values.push(OsString::from_vec(b"native\xff".to_vec()));
    }
    for value in values {
        let child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
            .env_clear()
            .env("PILOTDECK_GATEWAY_MODE", &value)
            .env("PILOTDECK_PROXY_LISTEN", "invalid-a4-address")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("#1354 invalid mode child launch");
        let (status, stderr) = collect_exit(child);
        assert!(
            !status.success(),
            "#1354 invalid mode {value:?} must not start: {status}; stderr: {stderr}"
        );
        assert!(
            stderr
                .contains("gateway mode (PILOTDECK_GATEWAY_MODE) must be exactly proxy or native."),
            "#1354 invalid mode {value:?} reports the explicit gate; stderr: {stderr}"
        );
    }
}

// The per-invocation `--native` argument outranks the config item, like the
// TS baseline's `readStringFlag(argv, "--host") ?? env.PILOTDECK_GATEWAY_HOST`
// (src/cli/pilotdeck.ts:213): with the config item set to `proxy`, the first
// argument still selects the native options parser (the fast-fail signature
// `tests/native_process.rs::explicit_entry_selects_parser` uses), never the
// proxy front — the unparseable listen address makes that fall-through
// observable without binding any port.
#[test]
fn argv_native_wins_over_the_proxy_mode_config() {
    let child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .arg("--native")
        .arg("--probe-invalid")
        .env_clear()
        .env("PILOTDECK_GATEWAY_MODE", "proxy")
        .env("PILOTDECK_PROXY_LISTEN", "invalid-a4-address")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("#1354 argv precedence child launch");
    let (status, stderr) = collect_exit(child);
    assert!(
        !status.success(),
        "#1354 argv precedence child must exit nonzero: {status}; stderr: {stderr}"
    );
    assert!(
        stderr.contains("unknown native option: --probe-invalid"),
        "#1354 --native must beat the proxy config item; stderr: {stderr}"
    );
}

// The mode gate is judged before the `--native` argument's routing, so an
// invalid value fails closed even when that argument is present (#1369
// shape): a `--native` invocation must not be able to smuggle a typo in the
// config item past startup. The unparseable listen address keeps a wrong
// fall-through to the proxy front observable without binding any port.
#[test]
fn invalid_mode_config_fails_closed_even_with_native_argv() {
    let child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .arg("--native")
        .env_clear()
        .env("PILOTDECK_GATEWAY_MODE", "prod")
        .env("PILOTDECK_PROXY_LISTEN", "invalid-a4-address")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("#1354 argv invalid mode child launch");
    let (status, stderr) = collect_exit(child);
    assert!(
        !status.success(),
        "#1354 --native must not bypass the mode gate: {status}; stderr: {stderr}"
    );
    assert!(
        stderr.contains("gateway mode (PILOTDECK_GATEWAY_MODE) must be exactly proxy or native."),
        "#1354 --native must not bypass the mode gate; stderr: {stderr}"
    );
}

// The rollback drill: both modes start, switch, and switch back on one
// isolated data directory. `PILOTDECK_PROXY_LISTEN` is always explicit
// (127.0.0.1:0), so no phase can ever touch a real listener.
#[tokio::test]
async fn both_modes_alternate_on_one_isolated_home() {
    let (_cleanup, root) = scratch_root("drill");
    let (home, project) = native_fixtures(&root);
    // The native assembly's project storage marker under the shared home
    // (`register_project` in src/native_process/assembly.rs): the file the
    // native phase writes at startup, and whose survival across the
    // rollback the drill asserts. `runs.json` cannot serve here — the run
    // registry only writes on its first run, and an idle server has none.
    let chats = pilotdeck_gateway::session_chat_dir(&home, &project, &project.to_string_lossy());
    let marker = chats
        .parent()
        .expect("#1354 project storage has a parent")
        .join(".cwd");

    // Phase 1: the default routing — the proxy front (the rollback target)
    // — serves from the isolated home.
    {
        let mut proxy = ProxyChild::start(&home, None);
        let address = proxy_advertises_address(&mut proxy);
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
        drop(proxy);
    }

    // Phase 2: switch to native over the same home (config item routing).
    native_child_serves(&root, |command| {
        command.env("PILOTDECK_GATEWAY_MODE", "native");
    })
    .await;
    assert!(
        marker.is_file(),
        "#1354 the native phase must have written its project storage marker into the shared home"
    );
    // The marker's bytes are the canonical project the native phase registered
    // (`register_project_at` writes `options.project`), so the drill carries
    // data content, not just file existence.
    let project_text = project.to_string_lossy().into_owned();
    let registered = std::fs::read_to_string(&marker)
        .expect("#1354 native phase marker must be readable")
        .trim()
        .to_owned();
    assert_eq!(
        registered, project_text,
        "#1354 the native phase's marker names the shared project"
    );

    // Phase 3: the rollback — the proxy front again over the same home, with
    // the native-written state still in place.
    {
        let mut proxy = ProxyChild::start(&home, None);
        let address = proxy_advertises_address(&mut proxy);
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
        drop(proxy);
    }
    // The proxy front never writes project storage; both the file and its
    // content must survive the rollback unchanged.
    let after_rollback = std::fs::read_to_string(&marker)
        .expect("#1354 rollback must leave the marker readable")
        .trim()
        .to_owned();
    assert_eq!(
        after_rollback, registered,
        "#1354 the rollback leaves the native-written state byte-identical"
    );

    // Phase 4: switch back to native over the same home.
    native_child_serves(&root, |command| {
        command.env("PILOTDECK_GATEWAY_MODE", "native");
    })
    .await;
}

/// One full native phase over the drill's directories: wait for the child's
/// own advertisement, prove steady state with a real WS 101 upgrade, then
/// SIGTERM and require the completed switch's clean exit (the #1353
/// machine; `tests/switch_e2e.rs` is the deeper contract of the same path).
/// The upgrade probe, the signal handoff and the completed-switch verdicts
/// are the shared segments in `tests/common/native_entry.rs` (#2511 item 1).
/// Returns the advertised address.
async fn native_child_serves(
    root: &std::path::Path,
    configure: impl FnOnce(&mut AsyncCommand),
) -> std::net::SocketAddr {
    let home = root.join("home");
    let project = root.join("project");
    let token_file = root.join("token");
    let sidecar = root.join("sidecar");
    let mut command = AsyncCommand::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"));
    command
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
        // The isolation claim this file makes everywhere ("no phase can ever
        // touch a real listener") holds for this child too: if the routing
        // ever regresses and the proxy front takes over, it must fail on the
        // unparseable listen address instead of binding the production
        // default port. The native parser never reads this variable.
        .env("PILOTDECK_PROXY_LISTEN", "invalid-a4-address")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    configure(&mut command);
    let mut child = command.spawn().expect("#1354 native child launch");
    let stderr = child.stderr.take().expect("#1354 native stderr pipe");
    // No `take()` here — `collect_exit`'s rule instead: a chatty child past
    // the stderr budget must still be drained, or it blocks on a full pipe
    // and never exits. Retention below is what the limit bounds.
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    // Wait for the child's own listening advertisement: past this point the
    // assembly is up and the signal handlers are registered.
    let mut advertisement = String::new();
    let advertised = timeout(ADVERTISEMENT_BUDGET, async {
        while let Some(line) = lines.next_line().await.expect("#1354 native stderr read") {
            if let Some(address) = advertised_address(&line) {
                advertisement = line.clone();
                return Some(address.to_owned());
            }
        }
        None
    })
    .await
    .expect("#1354 native advertisement deadline");
    let address_text = advertised.unwrap_or_else(|| {
        panic!("#1354 native never advertised listening (last line: {advertisement:?})")
    });
    let address: std::net::SocketAddr = address_text
        .parse()
        .expect("#1354 native advertised address parses");
    // Steady state is served: a real client completes the WS upgrade against
    // the advertised address (the shared switch_e2e observation, #2511
    // item 1).
    native_entry::steady_state_ws_upgrade(&address_text).await;
    // The signal the production entry is built around (the shared handoff,
    // #2511 item 1).
    native_entry::sigterm_child(&mut child).await;
    // Read to EOF (the child's exit ends the pipe) under the switch bound:
    // the tail is the evidence for which exit path ran. Retention is
    // bounded but the drain is not, so a chatty child still exits instead
    // of blocking on a full pipe.
    let mut tail = Vec::new();
    let mut retained = 0usize;
    timeout(SWITCH_DEADLINE, async {
        while let Some(line) = lines.next_line().await.expect("#1354 native tail read") {
            if retained < STDERR_LIMIT as usize {
                retained += line.len() + 1;
                tail.push(line);
            }
        }
    })
    .await
    .expect("#1354 native switch deadline");
    let status = timeout(SWITCH_DEADLINE, child.wait())
        .await
        .expect("#1354 native child exit deadline")
        .expect("#1354 native child exit status");
    // Positive evidence the signal really entered the switch machine (same
    // rule as switch_e2e), never a halt, and the clean exit: the shared
    // verdicts, with the tail asserted non-empty first so the "no halt
    // line" judgement cannot pass over zero output (#2511 item 1).
    native_entry::assert_completed_switch(&tail, &status);
    address
}
