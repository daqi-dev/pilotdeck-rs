//! Provenance checks shared by unit and integration fixture replays.
use pilotdeck_protocol::{JsValue, parse_json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

#[allow(dead_code)]
#[path = "regeneration_masks.rs"]
pub(crate) mod regeneration_masks;

const BASELINE_COMMIT: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("provenance string")
        .to_utf8()
        .expect("#988 provenance string must be valid UTF-8")
}

pub(crate) fn oracle(json: &str) -> JsValue {
    let oracle = parse_json(json).expect("oracle.json must be valid JSON");
    let baseline = std::env::var_os("PDRS_BASELINE");
    check(&oracle, baseline.as_deref().map(Path::new));
    oracle
}

pub(crate) fn check(oracle: &JsValue, baseline: Option<&Path>) {
    // Older corpora use commit/baselineCommit and sources. Keep their bytes and
    // schema intact while applying the same checks to every replay.
    let commit = ["sourceCommit", "baselineCommit", "commit"]
        .into_iter()
        .find_map(|key| oracle.get(key))
        .expect("oracle is missing its baseline commit");
    assert_eq!(text(commit), BASELINE_COMMIT, "oracle baseline commit");
    let hashes = oracle
        .get("sourceSha256")
        .or_else(|| oracle.get("sources"))
        .expect("oracle is missing sourceSha256/sources")
        .as_object()
        .expect("oracle source hashes must be an object");
    assert!(!hashes.is_empty(), "oracle source hashes must not be empty");
    if let Some(baseline) = baseline {
        let head = git(baseline, &["rev-parse", "HEAD"]);
        assert_eq!(
            String::from_utf8_lossy(&head).trim(),
            BASELINE_COMMIT,
            "PDRS_BASELINE must be checked out at the fixed commit"
        );
        let tracked = git(baseline, &["status", "--porcelain", "--untracked-files=no"]);
        assert!(
            tracked.is_empty(),
            "PDRS_BASELINE has uncommitted changes to tracked files:\n{}",
            String::from_utf8_lossy(&tracked)
        );
    }
    for (source, recorded) in hashes.entries() {
        let source = source.to_utf8().expect("UTF-8 source path");
        assert!(
            source.starts_with("src/") && source.split('/').all(|p| p != ".." && !p.is_empty()),
            "expected a baseline-relative source path: {source}"
        );
        let recorded = text(recorded);
        assert!(
            recorded.len() == 64 && recorded.bytes().all(|b| b.is_ascii_hexdigit()),
            "missing or malformed hash for {source}"
        );
        if let Some(baseline) = baseline {
            let committed = git(baseline, &["show", &format!("{BASELINE_COMMIT}:{source}")]);
            let expected = format!("{:x}", Sha256::digest(committed));
            assert_eq!(
                recorded, expected,
                "oracle hash for {source} does not match {BASELINE_COMMIT}"
            );
            let working =
                fs::read(baseline.join(&source)).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(
                format!("{:x}", Sha256::digest(working)),
                expected,
                "baseline source {source} differs from {BASELINE_COMMIT}"
            );
        }
    }
}

fn git(baseline: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args(["--no-replace-objects", "-C"])
        .arg(baseline)
        .args(args)
        .output()
        .expect("run git against PDRS_BASELINE");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Node generators share one environment contract, even on LC_* hosts.
#[allow(dead_code)]
pub(crate) fn node_command() -> Command {
    let mut command = Command::new("node");
    command
        .env("LANG", "en_US.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES");
    command
}

/// The directory a fixture's generator writes into, below its scratch root.
/// Historical layouts differ per fixture; nothing outside is touched.
fn generator_output_dir(fixture: &str, scratch: &Path) -> std::path::PathBuf {
    if fixture == "interaction-permission" {
        scratch.join("output")
    } else if fixture == "session-stale-handoff" {
        // #1020 fix-r1: the fixture README documents a direct invocation
        // whose OUT does not exist yet, so the guard must exercise that entry
        // instead of a pre-created scratch directory.
        scratch.join("out")
    } else {
        scratch.to_path_buf()
    }
}

/// The node invocation of `tests/fixtures/<fixture>/generate.mts` against the
/// real TS baseline, writing into `generator_output`.
fn generator_command(
    fixture: &str,
    scratch: &Path,
    generator_output: &Path,
    baseline: &Path,
) -> Command {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let node_flags: &[&str] = match fixture {
        "model-stream-google" => &["--stack-size=8192"],
        "session-overrides" => &["--expose-gc"],
        _ => &[],
    };
    let extra_args = if fixture == "private-ca" {
        vec![scratch.join("work")]
    } else {
        vec![]
    };
    let mut command = node_command();
    command
        .args(node_flags)
        .arg("--disable-warning=ExperimentalWarning")
        .arg("--import")
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(fixtures.join("generate.mts"))
        .arg(generator_output)
        .args(extra_args)
        .env("PDRS_BASELINE", baseline)
        .env(
            "PDRS_GIT",
            std::env::var_os("PDRS_GIT").unwrap_or_else(|| "/usr/bin/git".into()),
        );
    command
}

/// Re-runs `tests/fixtures/<fixture>/generate.mts` against the real TS
/// baseline (`PDRS_BASELINE`) into a fresh directory and requires a
/// byte-identical `oracle.json`, except the exact clock/PID paths documented
/// in `_shared/regeneration-masks.tsv`. Callers are `#[ignore]` tests.
#[allow(dead_code)]
pub(crate) fn regenerates_identically(fixture: &str) {
    let baseline = std::path::PathBuf::from(
        std::env::var("PDRS_BASELINE").expect("#280 set PDRS_BASELINE to the PilotDeck checkout"),
    );
    // Every invocation gets its own directory; nothing outside it is removed.
    let scratch = std::env::temp_dir().join(format!("pdrs-{fixture}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&scratch).expect("#280 create regeneration scratch");
    let generator_output = generator_output_dir(fixture, &scratch);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let mut command = generator_command(fixture, &scratch, &generator_output, &baseline);
    let (status, stdout, stderr) = match run_generator(&mut command, fixture) {
        Ok(output) => output,
        Err(message) => {
            // The child is already killed and reaped; remove the scratch this
            // test owns so a parked fixture cannot leak it.
            let _ = fs::remove_dir_all(&scratch);
            panic!("{message}");
        }
    };
    assert!(
        status.success(),
        "#280 {fixture}: generator failed\n{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    println!(
        "#280 {fixture} stdout:\n{}stderr:\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    if fixture == "session-stale-handoff" {
        // #1020 fix-r1: the generator removes its work-* scratch directories
        // before writing the oracle; leftovers would silently change what a
        // direct invocation leaves behind in OUT.
        let leftovers: Vec<String> = fs::read_dir(&generator_output)
            .expect("#1020 read generated output")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name() != "oracle.json")
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "#1020 {fixture}: generator left files behind in OUT: {leftovers:?}"
        );
    }
    let generated = generated_oracle_bytes(fixture, &scratch, &stdout)
        .unwrap_or_else(|error| panic!("#280 {fixture}: {error}"));
    let committed = fs::read(fixtures.join("oracle.json")).expect("#280 read committed oracle");
    // Keep the scratch on mismatch so the differing bytes can be inspected.
    regeneration_masks::compare(fixture, "oracle.json", &generated, &committed, (None, None))
        .unwrap_or_else(|error| {
            panic!(
                "#280 {fixture}/oracle.json differs from the regenerated {}: {error}",
                generator_output.join("oracle.json").display()
            )
        });
    compare_companion_files(fixture, &generator_output, &fixtures)
        .unwrap_or_else(|error| panic!("#280 {fixture}: {error}"));
    assert!(
        !generator_output.join("oracle.json.tmp").exists(),
        "#280 generator left a temporary file behind"
    );
    fs::remove_dir_all(&scratch).expect("#280 remove owned regeneration scratch");
}

/// sha256 of every regular file below `root`, keyed by its `/`-joined relative
/// path. Special node kinds are failures, not silently skipped entries.
fn digest_tree(root: &Path) -> std::collections::BTreeMap<String, String> {
    fn walk(root: &Path, prefix: &str, out: &mut std::collections::BTreeMap<String, String>) {
        for entry in fs::read_dir(root)
            .unwrap_or_else(|error| panic!("#1020 read {}: {error}", root.display()))
        {
            let entry = entry.expect("#1020 seeded directory entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry.file_type().expect("#1020 seeded entry file type");
            if kind.is_dir() {
                walk(&entry.path(), &format!("{prefix}{name}/"), out);
            } else if kind.is_file() {
                let bytes = fs::read(entry.path()).unwrap_or_else(|error| {
                    panic!("#1020 read {}: {error}", entry.path().display())
                });
                out.insert(
                    format!("{prefix}{name}"),
                    format!("{:x}", Sha256::digest(bytes)),
                );
            } else {
                panic!("#1020 {prefix}{name} is neither a regular file nor a directory");
            }
        }
    }
    let mut digests = std::collections::BTreeMap::new();
    walk(root, "", &mut digests);
    digests
}

/// #1020 fix-r2 (review P0): a direct invocation may pass an OUT that already
/// holds unrelated content, and the generator must remove only the work-*
/// roots it created itself. Seeds the fixture's output directory with
/// unrelated entries — including a `work-notes/` directory, exactly what the
/// fix-r1 prefix cleanup deleted — runs the generator, and returns every
/// seeded path that went missing or whose bytes changed (sha256 before vs
/// after). Callers are `#[ignore]` tests.
#[allow(dead_code)]
pub(crate) fn unrelated_out_changes_after_regeneration(fixture: &str) -> Vec<String> {
    let baseline = std::path::PathBuf::from(
        std::env::var("PDRS_BASELINE").expect("#280 set PDRS_BASELINE to the PilotDeck checkout"),
    );
    let scratch = std::env::temp_dir().join(format!("pdrs-{fixture}-{}", uuid::Uuid::new_v4()));
    let output = generator_output_dir(fixture, &scratch);
    fs::create_dir_all(output.join("work-notes")).expect("#1020 seed unrelated work-notes");
    fs::write(
        output.join("work-notes").join("keep.txt"),
        b"#1020 unrelated keep\n",
    )
    .expect("#1020 seed unrelated work-notes file");
    fs::write(
        output.join("unrelated.txt"),
        b"#1020 unrelated top-level file\n",
    )
    .expect("#1020 seed unrelated top-level file");
    let before = digest_tree(&output);
    let mut command = generator_command(fixture, &scratch, &output, &baseline);
    let (status, stdout, stderr) = match run_generator(&mut command, fixture) {
        Ok(output) => output,
        Err(message) => {
            let _ = fs::remove_dir_all(&scratch);
            panic!("{message}");
        }
    };
    if !status.success() {
        let _ = fs::remove_dir_all(&scratch);
        panic!(
            "#1020 {fixture}: generator failed on a pre-populated OUT\n{}{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    }
    assert!(
        output.join("oracle.json").is_file(),
        "#1020 {fixture}: oracle.json missing after the generator ran"
    );
    let after = digest_tree(&output);
    // Keep the scratch on mismatch so the differing bytes can be inspected.
    let changed: Vec<String> = before
        .iter()
        .filter_map(|(path, digest)| match after.get(path) {
            Some(actual) if actual == digest => None,
            Some(actual) => Some(format!("{path}: {digest} -> {actual}")),
            None => Some(format!("{path}: removed")),
        })
        .collect();
    if changed.is_empty() {
        fs::remove_dir_all(&scratch).expect("#1020 remove owned preservation scratch");
    }
    changed
}

/// Bound on reaping a killed process group and on each pipe drain after the
/// total deadline. A descendant that escaped the group with `setsid(2)` can
/// keep the inherited write ends open forever, so no wait after the deadline
/// may be unbounded.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Run a fixture generator child under a total deadline.
///
/// `Command::output()` waits forever when the fixture parks a cell before its
/// completion (a gated LLM request, a stuck predecessor). Poll `try_wait`
/// instead, keep both pipes drained on reader threads, and at the deadline
/// kill *and reap* the child's whole process group before reporting. The
/// deadline also covers the pipes: a child may exit while a forked descendant
/// still holds stdout/stderr, so reader joins are bounded too.
/// `PDRS_REGEN_TIMEOUT_SECS` overrides the 600-second default for slow hosts.
fn run_generator(
    command: &mut Command,
    fixture: &str,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
    let deadline = Duration::from_secs(
        std::env::var("PDRS_REGEN_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(600),
    );
    run_generator_with_deadline(command, fixture, deadline)
}

/// Testable core of [`run_generator`] with an explicit total deadline.
#[allow(dead_code)]
pub(crate) fn run_generator_with_deadline(
    command: &mut Command,
    fixture: &str,
    deadline: Duration,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
    let start = Instant::now();
    let deadline_at = start + deadline;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A dedicated process group lets the deadline terminate descendants
        // that inherited our stdout/stderr pipes, not only the direct child.
        command.process_group(0);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("#280 {fixture}: spawn TS generator: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("#280 {fixture}: capture generator stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("#280 {fixture}: capture generator stderr"))?;
    let drain = |mut pipe: std::process::ChildStdout| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = pipe.read_to_end(&mut buffer);
            buffer
        })
    };
    let stdout_reader = drain(stdout);
    let stderr_reader = thread::spawn(move || {
        let mut pipe = stderr;
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        buffer
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // A direct child can exit while a descendant still owns the
                // inherited write ends. Wait for EOF only inside the same total
                // deadline, then fall through to the process-group kill.
                if stdout_reader.is_finished() && stderr_reader.is_finished() {
                    let stdout = stdout_reader.join().unwrap_or_default();
                    let stderr = stderr_reader.join().unwrap_or_default();
                    return Ok((status, stdout, stderr));
                }
                if Instant::now() >= deadline_at {
                    return Err(generator_deadline_error(
                        &mut child,
                        fixture,
                        deadline,
                        start.elapsed(),
                        stdout_reader,
                        stderr_reader,
                    ));
                }
            }
            Ok(None) => {
                if Instant::now() >= deadline_at {
                    return Err(generator_deadline_error(
                        &mut child,
                        fixture,
                        deadline,
                        start.elapsed(),
                        stdout_reader,
                        stderr_reader,
                    ));
                }
            }
            Err(error) => {
                let pgid = child.id();
                let status = terminate_generator_tree(&mut child);
                let (stdout, stdout_stuck) = join_bounded(stdout_reader, DRAIN_GRACE);
                let (stderr, stderr_stuck) = join_bounded(stderr_reader, DRAIN_GRACE);
                return Err(format!(
                    "#280 {fixture}: poll TS generator: {error}; terminated process group \
                     {pgid} ({status:?}); stdout drain {}; stderr drain {}\nstdout:\n{}\nstderr:\n{}",
                    drain_state(stdout_stuck),
                    drain_state(stderr_stuck),
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                ));
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// Kill and reap the generator's process group, with every wait bounded.
fn terminate_generator_tree(child: &mut Child) -> Option<ExitStatus> {
    // The child was spawned with `process_group(0)` in `run_generator_with_deadline`,
    // so its pid is also the process-group id. Signalling the group terminates
    // every descendant that inherited the pipes; a vanished group is harmless.
    #[cfg(unix)]
    kill_process_group(child.id());
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    if let Some(status) = wait_bounded(child, DRAIN_GRACE) {
        return Some(status);
    }
    // The group signal can miss a raced leader; still bound the direct reap.
    let _ = child.kill();
    wait_bounded(child, DRAIN_GRACE)
}

/// Signal every process in `pgid`, without adding a crate dependency.
///
/// `std` exposes `CommandExt::process_group(0)` for spawning but no
/// group-signal API, and this module is copied verbatim into fixture negative
/// crates that only link std plus the harness's own externs (#879 r1 / #776
/// fix-r5). POSIX `kill(1)` with a negative pid signals the group; the helper
/// process is reaped with the same bounded wait as every other deadline path.
#[cfg(unix)]
fn kill_process_group(pgid: u32) {
    let mut command = Command::new("kill");
    command
        .args(["-KILL", "--", &format!("-{pgid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut killer) = command.spawn() {
        let _ = wait_bounded(&mut killer, DRAIN_GRACE);
    }
}

/// Reap `child` without ever waiting longer than `timeout`.
fn wait_bounded(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let until = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= until {
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Join one pipe reader, but never block past `timeout` on a pipe that a
/// detached descendant still holds open. A timed-out thread is left detached
/// (dropping its handle does not join); the process exits with the test binary.
fn join_bounded(reader: thread::JoinHandle<Vec<u8>>, timeout: Duration) -> (Vec<u8>, bool) {
    let until = Instant::now() + timeout;
    while !reader.is_finished() {
        if Instant::now() >= until {
            return (Vec::new(), true);
        }
        thread::sleep(Duration::from_millis(10));
    }
    (reader.join().unwrap_or_default(), false)
}

fn drain_state(stuck: bool) -> &'static str {
    if stuck {
        "timed out (pipe still held after the process-group kill)"
    } else {
        "reached EOF"
    }
}

fn generator_deadline_error(
    child: &mut Child,
    fixture: &str,
    deadline: Duration,
    elapsed: Duration,
    stdout_reader: thread::JoinHandle<Vec<u8>>,
    stderr_reader: thread::JoinHandle<Vec<u8>>,
) -> String {
    let pgid = child.id();
    let status = terminate_generator_tree(child);
    let (stdout, stdout_stuck) = join_bounded(stdout_reader, DRAIN_GRACE);
    let (stderr, stderr_stuck) = join_bounded(stderr_reader, DRAIN_GRACE);
    format!(
        "#280 {fixture}: generator exceeded the {deadline:?} total deadline after {elapsed:?}; \
         killed and reaped process group {pgid} ({status:?}); stdout drain {}; stderr drain {}\n\
         stdout:\n{}\nstderr:\n{}",
        drain_state(stdout_stuck),
        drain_state(stderr_stuck),
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    )
}

/// Additional generated artifacts consumed by replay tests must not silently
/// escape the byte comparison when the primary oracle happens to match.
#[allow(dead_code)]
pub(crate) fn compare_companion_files(
    fixture: &str,
    generated: &Path,
    committed: &Path,
) -> Result<(), String> {
    let names = companion_names(fixture, committed)?;
    let (generated_root, committed_root) = if fixture == "live-e2e" {
        let roots = (generated.join("expected"), committed.join("expected"));
        for root in [&roots.0, &roots.1] {
            if !fs::symlink_metadata(root)
                .map_err(|error| format!("#748 expected directory: {error}"))?
                .file_type()
                .is_dir()
            {
                return Err("#748 expected must be a regular directory".into());
            }
        }
        roots
    } else {
        (generated.to_path_buf(), committed.to_path_buf())
    };
    let pids = if fixture == "run-registry" {
        let read_pid = |root: &Path| -> Result<Option<u32>, String> {
            let bytes = fs::read(root.join("oracle.json"))
                .map_err(|error| format!("#280 read PID manifest: {error}"))?;
            regeneration_masks::pid(&bytes).map(Some)
        };
        (read_pid(generated)?, read_pid(committed)?)
    } else {
        (None, None)
    };
    for name in names {
        let path = generated_root.join(&name);
        let kind = fs::symlink_metadata(&path)
            .map_err(|error| format!("missing companion {name}: {error}"))?
            .file_type();
        if !kind.is_file() {
            return Err(format!("companion {name} must be a regular file"));
        }
        let actual = fs::read(path).map_err(|error| format!("read companion {name}: {error}"))?;
        let expected = fs::read(committed_root.join(&name))
            .map_err(|error| format!("read committed companion {name}: {error}"))?;
        regeneration_masks::compare(fixture, &name, &actual, &expected, pids)
            .map_err(|error| format!("companion {name} differs from committed bytes: {error}"))?;
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn companion_names(fixture: &str, committed: &Path) -> Result<Vec<String>, String> {
    let fixed: &[&str] = match fixture {
        "live-e2e" => &[
            "text.json",
            "tool.json",
            "cancel-stream.json",
            "cancel-tool.json",
            "drop.json",
            "duplicate.json",
            "early-final.json",
        ],
        "interaction-events" => &["source.json"],
        "interaction-permission" => &["source.json", "profiles.json", "hooks.json"],
        "stats-dateparse" => &["local-profiles.json"],
        // The generator emits this kernel-dependent oracle only on darwin;
        // other hosts compare the portable oracle.json without this companion.
        "skills-tree-files" if cfg!(target_os = "macos") => &["oracle-macos.json"],
        "user-dialog" => &["hosted.json"],
        "upload" => &[
            "created.json",
            "uploading.json",
            "completed.json",
            "lease.json",
            "expired.json",
        ],
        _ => &[],
    };
    let mut names: Vec<_> = fixed.iter().map(|name| (*name).to_owned()).collect();
    if matches!(fixture, "operation-ledger" | "run-registry") {
        let json = fs::read_to_string(committed.join("oracle.json")).map_err(|e| e.to_string())?;
        let oracle = parse_json(&json).map_err(|e| format!("companion manifest JSON: {e:?}"))?;
        let mut add = |value: Option<&JsValue>| -> Result<(), String> {
            names.push(
                value
                    .and_then(JsValue::as_string)
                    .ok_or("companion manifest filename must be a string")?
                    .to_utf8()
                    .map_err(|e| format!("companion filename UTF-8: {e:?}"))?,
            );
            Ok(())
        };
        let groups: &[&str] = if fixture == "operation-ledger" {
            &["cases", "rejectedCases", "seedCases"]
        } else {
            &["scenarios"]
        };
        for group in groups {
            let Some(JsValue::Array(rows)) = oracle.get(group) else {
                return Err(format!("companion manifest missing {group} array"));
            };
            for row in rows {
                if fixture == "operation-ledger" {
                    add(row.get("file"))?;
                } else {
                    for key in ["initial", "after"] {
                        if let Some(value) = row.get(key).filter(|v| !matches!(v, JsValue::Null)) {
                            add(Some(value))?;
                        }
                    }
                }
            }
        }
        if fixture == "operation-ledger" {
            add(oracle.get("utf8Read").and_then(|row| row.get("file")))?;
            names.push("partial-continued.jsonl".into());
        }
        let expected = if fixture == "operation-ledger" {
            34
        } else {
            49
        };
        if names.len() != expected {
            return Err(format!(
                "companion manifest count: expected {expected}, got {}",
                names.len()
            ));
        }
    }
    let mut unique = std::collections::HashSet::new();
    for name in &names {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains(['/', '\\'])
            || !unique.insert(name)
        {
            return Err(format!("invalid or duplicate companion filename {name:?}"));
        }
    }
    Ok(names)
}

/// Historical output layouts; collect all candidates without following links.
#[allow(dead_code)]
pub(crate) fn generated_oracle_bytes(
    fixture: &str,
    directory: &Path,
    stdout: &[u8],
) -> Result<Vec<u8>, String> {
    if matches!(fixture, "interaction-events" | "turn-lifecycle-matrix") {
        return Ok(stdout.to_vec());
    }
    fn collect(
        directory: &Path,
        rewind: bool,
        found: &mut Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                collect(&entry.path(), rewind, found)?;
            } else if kind.is_file() {
                if entry.file_name() == "oracle.json.tmp" {
                    return Err(std::io::Error::other(
                        "generator left oracle.json.tmp behind",
                    ));
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == "oracle.json"
                    || (rewind && name.starts_with("rewind-oracle-") && name.ends_with(".json"))
                {
                    found.push(entry.path());
                }
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    collect(directory, fixture == "rewind", &mut found).map_err(|error| error.to_string())?;
    if found.len() != 1 {
        return Err(format!(
            "expected exactly one generated oracle in {}, found {}",
            directory.display(),
            found.len()
        ));
    }
    fs::read(&found[0]).map_err(|error| error.to_string())
}
