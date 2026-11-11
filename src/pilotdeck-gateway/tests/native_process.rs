#[allow(dead_code)]
#[path = "../src/process_group.rs"]
mod process_group;
#[allow(dead_code)]
// Module name matters: `src/test_process.rs` calls
// `crate::test_exact_receipt::single_pass`, and these integration crates
// `#[path]`-include that file, so each crate root must declare the same
// module name (the lib declares it from `tests/common/exact_receipt.rs`).
#[path = "common/exact_receipt.rs"]
mod test_exact_receipt;
#[allow(dead_code)]
#[path = "common/negative_receipt.rs"]
mod test_negative_receipt;
#[allow(dead_code)]
#[path = "common/permission_faults.rs"]
mod test_permission_faults;
#[allow(dead_code)]
#[path = "../src/test_process.rs"]
mod test_process;
#[allow(dead_code)]
#[path = "common/skip_line.rs"]
mod test_skip_line;
// Kept for the `#[path]`-included test_process.rs above, whose own tests
// resolve `crate::test_scratch` through this crate root; this file's own
// tests stopped using it (#1401 item 5).
#[path = "common/scratch.rs"]
mod test_scratch;
// The one shared `VALID_HOME_CONFIG` encoding (#2510 item 2): this file's
// own third copy is gone; only its needle-pinned advertisement spellings
// stay local (frozen `native-entry-authority` binding, see that module's
// header for why they cannot migrate).
#[allow(dead_code)]
#[path = "common/native_entry.rs"]
mod native_entry;
use native_entry::VALID_HOME_CONFIG;
// Real binaries, isolated state, Node observer and local-only provider.
use std::{path::PathBuf, process::Command};
#[test]
#[ignore = "build gateway+sidecar bins first; requires Node v22.22.0 and the pinned TS baseline"]
fn executable_native_and_a4() {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .expect("#1071 explicit CARGO_TARGET_DIR");
    let sidecar = target.join("debug/pilotdeck-agent-loop-sidecar");
    assert!(
        sidecar.is_file(),
        "#1071 build the real sidecar binary first"
    );
    let baseline = std::env::var_os("PDRS_BASELINE")
        .map(PathBuf::from)
        .expect("#1071 explicit read-only baseline");
    let output = std::env::var_os("PDRS_1071_PROCESS_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // The native child canonicalizes --project/--home, but the
            // observer echoes the configured project path back as the
            // projectKey. On macOS the default temp dir sits behind the
            // /var -> /private/var symlink, so a raw temp_dir() root would
            // stop equaling its canonical self; hand the observer a
            // canonical root (the scratch-root precedent in
            // background_tasks.rs).
            std::fs::canonicalize(std::env::temp_dir())
                .expect("#1071 canonical temp evidence root")
                .join(format!("pdrs-1071-process-{}", uuid::Uuid::new_v4()))
        });
    assert!(!output.exists(), "#1071 fresh process evidence directory");
    let result = Command::new("node")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/native-process/process.mts"),
        )
        .arg(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
        .arg(sidecar)
        .arg(&output)
        .output()
        .expect("#1071 launch process observer");
    print!("{}", String::from_utf8_lossy(&result.stdout));
    eprint!("{}", String::from_utf8_lossy(&result.stderr));
    println!(
        "#1071 process evidence {} (caller owns cleanup)",
        output.display()
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(output.join("process-record.json")).expect("#1071 process observer record"),
    )
    .expect("#1071 process observer JSON");
    let assertion = record["assertionFailure"].as_str();
    // The dedicated classified assertions must run before the generic nonzero
    // exit oracle: a mutation bound to one of them records its receipt and
    // panics here, while an unrelated earlier failure (timeout, another
    // scenario) only trips the unbound generic oracle and stays INVALID.
    negative_assert!(
        assertion != Some("#1071 clean signal exit"),
        "#1071 executable clean signal exit"
    );
    negative_assert!(
        assertion != Some("#1071 refreshed hooks refused"),
        "#1071 executable refreshed hooks refused"
    );
    negative_assert!(
        assertion != Some("#1131 readiness reports the kernel-assigned port"),
        "#1131 executable readiness reports the kernel-assigned port"
    );
    negative_assert!(
        assertion != Some("#1071 readiness address carries a port"),
        "#1071 executable readiness address carries a port"
    );
    negative_assert!(
        result.status.success(),
        "#1071 executable native and A4 oracle"
    );
}

#[tokio::test]
async fn explicit_entry_selects_parser() {
    for (args, native) in [
        (vec!["--native", "--probe-invalid"], true),
        (vec!["a4", "--native", "--probe-invalid"], false),
        (vec!["--native=true", "--probe-invalid"], false),
    ] {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
                .args(&args)
                .env_clear()
                .env("PILOTDECK_PROXY_LISTEN", "invalid-a4-address")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("#1071 entry parser process deadline")
        .expect("#1071 entry parser process launch");
        assert!(
            !output.status.success(),
            "#1071 invalid parser input exits nonzero"
        );
        let error = String::from_utf8(output.stderr).expect("#1071 entry stderr UTF8");
        println!("#1071 entry args={args:?}: {error}");
        if native {
            negative_assert!(
                error.contains("unknown native option: --probe-invalid"),
                "#1071 native first argument selects native parser"
            );
        } else {
            negative_assert!(
                !error.contains("native option") && error.contains("AddrParseError(Socket)"),
                "#1071 nonfirst native argument keeps A4 parser"
            );
        }
    }
}

// The child's own stderr line once it is serving, exactly as the product
// writes it (`console_log_line` on stderr in src/native_process.rs; the
// #2558 fix changed only the panic behavior of the write, not the bytes).
// Observations anchor on the whole line (see
// `advertises`): a substring match would also fire on a diagnostic that merely
// mentions the phrase.
const NATIVE_LISTENING: &str = "native listening";
// How long a child may say nothing at all, and the absolute budget for one
// observation. The silence bound alone would let a child that logs once a
// minute hold the read forever; the budget is generous because a slow host
// still says something (#1401 review, both lineages). Silence is unjudgeable,
// never a kill.
const CHILD_SILENCE_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
const CHILD_OBSERVATION_BUDGET: std::time::Duration = std::time::Duration::from_secs(180);
// A rejected child exits by itself right after printing the rejection (the
// measured median is 4.6 ms, README), so this only bounds a child that hangs
// on its way out.
const CHILD_EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
// A child that prints more than this before the observation resolves is not
// the shape either test describes; the read stops rather than growing without
// bound.
const CHILD_STDERR_LIMIT: usize = 64 * 1024;

// Whether one *finished* stderr line is the product's listening
// advertisement: the phrase, a space, then a non-empty address with no
// whitespace of its own, then the newline `eprintln!` writes. Anything else
// -- the bare phrase, a longer diagnostic that merely begins with it, junk
// after the address, or a line EOF or the byte bound cut short -- is not
// proof that something is serving (#1401 review P0).
fn advertises(line: &[u8]) -> bool {
    if !line.ends_with(b"\n") {
        return false;
    }
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end_matches(&['\r', '\n'][..]);
    match text
        .strip_prefix(NATIVE_LISTENING)
        .map(|rest| rest.strip_prefix(' '))
    {
        Some(Some(address)) => !address.is_empty() && !address.contains(char::is_whitespace),
        _ => false,
    }
}

// Pins the shape `advertises` accepts, so the P0 above cannot come back as a
// silent loosening: every line in the second list was, at some point in this
// branch's history, judged as an advertisement.
#[test]
fn native_listening_advertisement_shape() {
    for line in [
        "native listening 127.0.0.1:4321\n",
        "native listening [::1]:4321\r\n",
    ] {
        assert!(
            advertises(line.as_bytes()),
            "must count as an advertisement: {line:?}"
        );
    }
    for line in [
        "native listening\n",
        "native listening \n",
        "native listening ::1:4321",
        "native listening 127.0.0.1:4321 extra\n",
        "native listening: 127.0.0.1:4321\n",
        "warn: native listening 127.0.0.1:4321\n",
        "",
    ] {
        assert!(
            !advertises(line.as_bytes()),
            "must not count as an advertisement: {line:?}"
        );
    }
}

// The observation function's own edges: a finished advertisement counts, and
// an unfinished one -- EOF without the newline, a line the byte bound cut --
// or silence does not (#1401 review P0: the branch that judged a half line as
// an advertisement had no test that could see it).
#[tokio::test]
async fn native_listening_observation_needs_a_finished_line() {
    use tokio::io::AsyncWriteExt;
    let silence = std::time::Duration::from_millis(50);
    let budget = std::time::Duration::from_secs(5);

    let (collected, advertised) =
        read_advertisement(&b"native listening 127.0.0.1:4321\n"[..], silence, budget).await;
    assert!(advertised, "finished advertisement: {collected:?}");

    // The same bytes without the newline: EOF left the line unfinished, so the
    // child never completed the product's `eprintln!` line.
    let (collected, advertised) =
        read_advertisement(&b"native listening 127.0.0.1:4321"[..], silence, budget).await;
    assert!(!advertised, "line cut by EOF: {collected:?}");

    // The byte bound cuts a single unterminated line: its advertisement-
    // shaped prefix must not be accepted, and the buffer must stop growing.
    let mut stream = b"native listening ".to_vec();
    stream.resize(stream.len() + CHILD_STDERR_LIMIT * 2, b'1');
    let (collected, advertised) = read_advertisement(&stream[..], silence, budget).await;
    assert!(
        !advertised,
        "line cut by the byte bound: {} bytes",
        collected.len()
    );
    assert!(
        collected.len() <= CHILD_STDERR_LIMIT,
        "collected {} bytes",
        collected.len()
    );

    // Silence resolves at the silence bound, not at the total budget. The
    // elapsed bound separates the 50 ms silence from the 5 s budget, so a
    // swapped pair of bounds at the call site -- or `timeout(left, ...)`
    // instead of `timeout(silence.min(left), ...)` -- fails here instead of
    // silently turning a 60 s child limit into the whole budget (#1989 item 5).
    let started = std::time::Instant::now();
    let (mut writer, reader) = tokio::io::duplex(64);
    let (collected, advertised) = read_advertisement(reader, silence, budget).await;
    assert!(!advertised, "silence: {collected:?}");
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "silence bound, not the total budget: {elapsed:?}"
    );
    let _ = writer.write_all(b"late").await;

    // A read error is unjudgeable, never an advertisement: the bytes read
    // before it stay as evidence and the verdict is false. Nothing else
    // reaches the error arm, so a folded `Ok(Err(_))` could only show here
    // (#1989 item 5).
    let (collected, advertised) = read_advertisement(
        ErrorAfter::new(b"native listening 127.0.0.1:4321"),
        silence,
        budget,
    )
    .await;
    assert!(!advertised, "read error: {collected:?}");
    assert!(
        collected.ends_with(b"127.0.0.1:4321"),
        "read error keeps the bytes read before it: {collected:?}"
    );

    // A child that prints complete, non-advertisement lines and then stops
    // talking never advertises and never goes silent, so only the total budget
    // can end the observation -- and `budget` / `silence` are swapped here on
    // purpose (50 ms budget, 5 s silence). The elapsed bound is far below 5 s,
    // so a read that lost the budget would sit out the whole silence bound
    // (#1989 item 5). Three lines cannot reach the 64 KiB byte bound, so this
    // case ends on the budget and not on the buffer limit.
    let started = std::time::Instant::now();
    let (collected, advertised) =
        read_advertisement(TalkativeLines::lines(3), budget, silence).await;
    assert!(!advertised, "budget: {} bytes", collected.len());
    assert!(
        collected.ends_with(b"noise from a child that never advertises\n"),
        "the budget case read whole non-advertisement lines: {collected:?}"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the total budget ends a talkative child: {elapsed:?}"
    );
}

// Test-only streams for the read-error and total-budget cases (#1989 item 5),
// so both branches are reachable without a child process. `ErrorAfter` fails
// once its bytes are consumed; `TalkativeLines` prints a few complete
// non-advertisement lines and then stops producing (returning `Pending`, so
// the read waits for its timeout instead of spinning). A run that lost the
// total budget therefore pays one silence timeout and then fails its elapsed
// bound, instead of hanging.
struct ErrorAfter {
    bytes: std::io::Cursor<&'static [u8]>,
}

impl ErrorAfter {
    fn new(bytes: &'static [u8]) -> Self {
        Self {
            bytes: std::io::Cursor::new(bytes),
        }
    }
}

impl tokio::io::AsyncRead for ErrorAfter {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.bytes.position() as usize >= this.bytes.get_ref().len() {
            return std::task::Poll::Ready(Err(std::io::Error::other("#1989 injected read error")));
        }
        std::pin::Pin::new(&mut this.bytes).poll_read(cx, buf)
    }
}

struct TalkativeLines {
    remaining: usize,
}

impl TalkativeLines {
    fn lines(lines: usize) -> Self {
        Self { remaining: lines }
    }
}

impl tokio::io::AsyncRead for TalkativeLines {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            return std::task::Poll::Pending;
        }
        this.remaining -= 1;
        let line = b"noise from a child that never advertises\n";
        buf.put_slice(&line[..line.len().min(buf.remaining())]);
        std::task::Poll::Ready(Ok(()))
    }
}

// Reads stderr until the child advertises listening.
//
// Returns as soon as a line is an advertisement, so a child that stays up
// afterwards costs one line, not a whole deadline (#1401 review P2:
// `read_to_end` under a 5 s deadline spent all 5 s on every intact run and
// read a slow host as a missing advertisement). Otherwise the bytes collected
// before EOF, a read error, the silence bound, the total budget or the byte
// bound come back with `false`.
//
// Generic over the stream and parameterized over both bounds so the
// function-level control below can drive it with a byte slice, a long
// unterminated line and a duplex pipe instead of a real child: the EOF,
// truncation and silence branches had no test that could see them
// (#1401 review P0).
async fn read_advertisement<R>(
    stderr: R,
    silence: std::time::Duration,
    budget: std::time::Duration,
) -> (Vec<u8>, bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let started = std::time::Instant::now();
    // `take` bounds the stream itself, not just what this loop keeps:
    // `read_until` appends to its buffer until it sees a newline or EOF, so a
    // child that streams one unterminated line would otherwise grow that
    // single buffer without limit until the deadline (#1401 review P0).
    let mut reader = tokio::io::BufReader::new(stderr).take(CHILD_STDERR_LIMIT as u64);
    let mut line = Vec::new();
    let mut collected = Vec::new();
    loop {
        let left = budget.saturating_sub(started.elapsed());
        if left.is_zero() {
            return (collected, false);
        }
        line.clear();
        let read = reader.read_until(b'\n', &mut line);
        match tokio::time::timeout(silence.min(left), read).await {
            // EOF, read error, silence, budget and the byte bound are one
            // case: nothing more will be observed, and a line this read did
            // not finish is not an advertisement. A child that prints the
            // phrase and then hangs (or dies) has proven nothing (#1401
            // review P0). What the cancelled read already appended is kept as
            // evidence for the caller's own checks.
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => {
                collected.extend_from_slice(&line);
                return (collected, false);
            }
            Ok(Ok(_)) => {
                let advertised = advertises(&line);
                collected.extend_from_slice(&line);
                if advertised {
                    return (collected, true);
                }
            }
        }
    }
}

// The observation both tests wait for: the child's own listening
// advertisement, under the shared bounds. Kept as one unformatted call per
// test because the fixture's `--check` binds this call inside each test body.
async fn observe_child_stderr(stderr: tokio::process::ChildStderr) -> (Vec<u8>, bool) {
    read_advertisement(stderr, CHILD_SILENCE_LIMIT, CHILD_OBSERVATION_BUDGET).await
}

// #1401 item 5: these tests bring their own scratch root so every failure
// point carries the #1261 message instead of the shared helper's plain
// expects. The owner token the shared `Scratch` also carries is not needed
// here: the path is unique per run and nothing else ever writes it.
struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_root(tag: &str) -> (Cleanup, std::path::PathBuf) {
    let root =
        std::env::temp_dir().join(format!("pdrs-1261-native-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).expect("#1261 native entry scratch directory must be new");
    (Cleanup(root.clone()), root)
}

// kill_on_drop only sends SIGKILL without waiting, so an explicit kill + wait
// before the scratch root is removed keeps the removal from racing a child
// that is still writing under home (#1401 review).
async fn reap(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

// #1261: anchor run()'s validate_environment call site, not the function body
// (production_entry_snapshot_capabilities_are_disabled already mirrors the
// combination by calling the function directly). Only the real --native child
// observes the call site, and each way to displace the call has its own
// observation: a removed call lets the child reach its own listening
// advertisement, and a reordered call lets assembly's project registration
// leave entries under home before the rejection. Launch, pipe, decode and
// early-exit failures stay unbound (INVALID): a child that says neither the
// rejection nor the advertisement says nothing about the gate.
#[tokio::test]
async fn native_entry_process_rejects_inherited_authority() {
    let (_cleanup, root) = scratch_root("entry");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir(&home).expect("#1261 native entry home directory");
    std::fs::create_dir(&project).expect("#1261 native entry project directory");
    let token_file = root.join("token");
    std::fs::write(&token_file, "1261-native-entry-token\n")
        .expect("#1261 native entry token file");
    let sidecar = root.join("sidecar");
    std::fs::write(&sidecar, "#1261 never executed before the rejection\n")
        .expect("#1261 native entry sidecar file");
    std::fs::write(home.join("pilotdeck.yaml"), VALID_HOME_CONFIG)
        .expect("#1261 native entry home config file");
    for key in [
        "PILOTDECK_STORAGE_ROOT",
        "PILOTDECK_SNAPSHOT_ROOT",
        "PILOTDECK_MANAGED_CONFIG",
    ] {
        let rejection = format!("native isolated mode does not accept inherited {key}");
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
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
            .env_clear()
            .env(key, root.join("inherited-authority"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("#1261 native entry child launch");
        let stderr = child
            .stderr
            .take()
            .expect("#1261 native entry child stderr pipe");
        let (collected, advertised) = observe_child_stderr(stderr).await;
        let stderr = String::from_utf8_lossy(&collected);
        // The advertisement alone proves the entry gate accepted the inherited
        // authority, so this marked assertion covers every shape that prints
        // it -- including a child that exits right after (#1401 item 1).
        negative_assert!(!advertised, "#1261 native entry rejects inherited {key}");
        // Neither the rejection nor the advertisement: the child exited (or
        // went quiet) before the entry gate, so this run cannot tell an intact
        // gate from a displaced one. The plain panic carries no #1261 kill
        // marker: the judge records INVALID instead of a false KILLED.
        if !stderr.contains(&rejection) {
            panic!(
                "#1261 native entry child exited early; no rejection and no listening advertisement for {key}, cannot judge"
            );
        }
        // The rejection is on stderr, so the child is on its way out.
        let status = tokio::time::timeout(CHILD_EXIT_DEADLINE, child.wait())
            .await
            .expect("#1261 native entry child exit deadline")
            .expect("#1261 native entry child exit status");
        negative_assert!(
            !status.success(),
            "#1261 native entry rejects inherited {key}"
        );
        // A reordered call reaches assembly before the rejection and
        // leaves its project registration and run registry under
        // home; the entry gate leaves home exactly as this test
        // wrote it (only the config file above).
        let mut leftover: Vec<_> = std::fs::read_dir(&home)
            .expect("#1261 native entry home still readable")
            .map(|entry| entry.expect("#1261 native entry home entry").file_name())
            .filter(|name| name.as_os_str() != std::ffi::OsStr::new("pilotdeck.yaml"))
            .collect();
        leftover.sort();
        println!("#1261 leftover under home for {key}: {leftover:?}");
        negative_assert!(
            leftover.is_empty(),
            "#1261 native entry rejects inherited {key}"
        );
        reap(&mut child).await;
    }
}

// #1401 item 6: anchor run()'s options.environment call site. The unit
// tests in src/native_process/tests.rs call the function directly; only
// the real --native child exercises the call site. environment() removes
// PILOTDECK_CONFIG_PATH, so the isolated home's config wins
// (config global_config_path); with the call displaced
// (`let env = inherited;`) the inherited key survives, the child loads
// the injected invalid YAML instead, and fails fast with the
// default-config parse error — the only observation this failure can
// produce, because the error conversion drops the diagnostics that
// would have carried the path (measured on cb8: the message text is the
// whole evidence). With the call intact the child reads the valid home
// config and stays up, advertising listening. Neither the advertisement
// nor a silence kills on its own: only the parse error does, so a slow
// host can only ever be INVALID.
#[tokio::test]
async fn native_entry_scrubs_inherited_process_environment() {
    let (_cleanup, root) = scratch_root("scrub");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir(&home).expect("#1261 native entry scrub home directory");
    std::fs::create_dir(&project).expect("#1261 native entry scrub project directory");
    let token_file = root.join("token");
    std::fs::write(&token_file, "1261-native-scrub-token\n")
        .expect("#1261 native entry scrub token file");
    let sidecar = root.join("sidecar");
    std::fs::write(
        &sidecar,
        "#1261 never executed before the scrub observation\n",
    )
    .expect("#1261 native entry scrub sidecar file");
    std::fs::write(home.join("pilotdeck.yaml"), VALID_HOME_CONFIG)
        .expect("#1261 native entry scrub home config file");
    // Invalid on purpose: with the call site displaced the child must fail
    // fast on it; with the call intact the key is removed and this file is
    // never read.
    let inherited_config = root.join("inherited-config.yaml");
    std::fs::write(&inherited_config, "not: valid: yaml: {{{\n")
        .expect("#1261 native entry scrub inherited config file");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
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
        .env_clear()
        .env("PILOTDECK_CONFIG_PATH", &inherited_config)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("#1261 native entry scrub child launch");
    let stderr = child
        .stderr
        .take()
        .expect("#1261 native entry scrub child stderr pipe");
    let (collected, advertised) = observe_child_stderr(stderr).await;
    let stderr = String::from_utf8_lossy(&collected);
    // The inherited key surviving the scrub is the only kill: the child then
    // parses the injected file and exits with this message.
    let leaked_parse = stderr.contains("Failed to parse default config YAML.");
    if leaked_parse {
        println!("#1261 scrub early exit: {stderr}");
    }
    negative_assert!(
        !leaked_parse,
        "#1261 native entry scrubs inherited PILOTDECK_CONFIG_PATH"
    );
    // Intact: the key was scrubbed, the home config loaded and the child is
    // up. Silence with neither observation is unjudgeable (INVALID).
    if !advertised {
        panic!(
            "#1261 native entry scrub child neither advertised listening nor exited, cannot judge"
        );
    }
    println!("#1261 scrub child advertised listening; inherited config was not read");
    reap(&mut child).await;
}
