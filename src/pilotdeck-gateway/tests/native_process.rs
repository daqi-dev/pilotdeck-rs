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
#[path = "../src/test_process.rs"]
mod test_process;
// Kept for the `#[path]`-included test_process.rs above, whose own tests
// resolve `crate::test_scratch` through this crate root; this file's own
// tests stopped using it (#1401 item 5).
#[path = "common/scratch.rs"]
mod test_scratch;
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

// The child's own stderr line once it is serving. Observations anchor on the
// whole line (see read_stderr_until): a substring match would also fire on a
// diagnostic that merely mentions the phrase.
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

// Reads a child's stderr line by line and returns as soon as a line starts
// with `needle`, so a child that stays up after advertising listening costs
// one line, not a whole deadline (#1401 review P2: `read_to_end` under a 5 s
// deadline spent all 5 s on every intact run and read a slow host as a
// missing advertisement). Otherwise the bytes collected before EOF, a read
// error, the silence bound or the total budget come back with `false`.
//
// `read_until` is not cancel safe, so a cancelled read may leave the line it
// was building half-filled; those bytes are part of what the child said and
// are returned with the rest (a final line without a newline therefore still
// reaches the caller's own needle checks), and the same line is judged for
// the advertisement -- fail-closed either way, never a false kill.
async fn read_stderr_until(
    stderr: tokio::process::ChildStderr,
    needle: &str,
    silence: std::time::Duration,
    budget: std::time::Duration,
) -> (Vec<u8>, bool) {
    use tokio::io::AsyncBufReadExt;
    let started = std::time::Instant::now();
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut line = Vec::new();
    let mut collected = Vec::new();
    loop {
        let left = budget.saturating_sub(started.elapsed());
        if left.is_zero() {
            return (collected, false);
        }
        line.clear();
        match tokio::time::timeout(silence.min(left), reader.read_until(b'\n', &mut line)).await {
            // EOF, read error, silence and budget are one case: no more
            // observations and no advertisement, which every caller judges as
            // INVALID. Whatever the cancelled read already appended is kept
            // (`read_until` appends block by block), so a child that prints
            // its last line without a newline still leaves its evidence with
            // the caller instead of turning a judgeable exit into silence.
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => {
                collected.extend_from_slice(&line);
                let advertised = line.starts_with(needle.as_bytes());
                return (collected, advertised);
            }
            Ok(Ok(_)) => {
                if line.starts_with(needle.as_bytes()) {
                    collected.extend_from_slice(&line);
                    return (collected, true);
                }
                if collected.len() + line.len() > CHILD_STDERR_LIMIT {
                    return (collected, false);
                }
                collected.extend_from_slice(&line);
            }
        }
    }
}

// The observation both tests wait for: the child's own listening
// advertisement, under the shared bounds. Kept as one unformatted call per
// test because the fixture's `--check` binds this call inside each test body.
async fn observe_child_stderr(stderr: tokio::process::ChildStderr) -> (Vec<u8>, bool) {
    read_stderr_until(
        stderr,
        NATIVE_LISTENING,
        CHILD_SILENCE_LIMIT,
        CHILD_OBSERVATION_BUDGET,
    )
    .await
}

// One complete valid profile for both child tests: with the entry gate or the
// scrub displaced the child must not fail on anything else, so the
// observations stay about the call site alone.
const VALID_HOME_CONFIG: &str = "schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\n";

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
