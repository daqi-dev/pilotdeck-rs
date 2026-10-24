//! Exercise request dumping through the real OpenAI and Google send paths.
//!
//! Each case runs in its own subprocess (no unsafe `set_var`/`remove_var`)
//! against a real mock transport, so `PILOTDECK_DUMP_REQUEST` is set exactly
//! for the code under test. The child's whole temp directory is an
//! exclusively acquired scratch directory of this run, so the parent can
//! prove dumping from the filesystem — it counts `pilotdeck_request_*`
//! entries there — instead of inferring it from the child's logs, and no
//! other process's temp entries are ever read or removed. The parent
//! verifies the exact dump paths the child reported and removes only the
//! scratch directory it created itself.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use pilotdeck_gateway::runtime::model::http::{
    ModelHttpTransport, StreamOptions, StreamOutput, TransportFailure, TransportFuture,
    complete_model, stream_model,
};
use pilotdeck_protocol::parse_json;
use serde_json::{Value, json};
use std::process::Command;

#[derive(Default)]
struct CaptureTransport(std::sync::Arc<std::sync::Mutex<Vec<Value>>>);

impl ModelHttpTransport for CaptureTransport {
    fn send(&self, request: Request<Full<Bytes>>) -> TransportFuture {
        let bodies = self.0.clone();
        TransportFuture::io(async move {
            let bytes = request.into_body().collect().await.unwrap().to_bytes();
            bodies
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&bytes).unwrap());
            // Dumping happens before the exchange; no real model or network needed.
            Err(TransportFailure::fetch_failed(Some("ECONNREFUSED")))
        })
    }
}

/// A temp directory this run acquired exclusively (fresh UUID name, so
/// nothing pre-existing can be under it; no predictable name is probed or
/// deleted) for one child case. Only what this guard created is removed.
struct CaseRoot(std::path::PathBuf);
impl CaseRoot {
    fn acquire(tag: &str) -> Self {
        for _ in 0..8 {
            let dir =
                std::env::temp_dir().join(format!("pdrs-dump-case-{tag}-{}", uuid::Uuid::new_v4()));
            // Exclusive create (no create_dir_all): a missing parent fails
            // loudly instead of reaching into an unexpected tree.
            if std::fs::create_dir(&dir).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                }
                return Self(dir);
            }
            // A collision is not ours: try the next UUID, never delete it.
        }
        panic!("dump case: could not exclusively create a scratch temp directory");
    }
}
impl Drop for CaseRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl std::ops::Deref for CaseRoot {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The dump paths the child reported on stdout, in order. Parsing the child's
/// own output is the only source: the parent never guesses or globs paths.
fn reported_dump_paths(stdout: &str) -> Vec<std::path::PathBuf> {
    stdout
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("[model-debug] Request dumped to ")?;
            let path = rest.strip_suffix(" (model=dump-model)")?;
            Some(std::path::PathBuf::from(path))
        })
        .collect()
}

/// The `pilotdeck_request_*` entries the child actually left in the exclusive
/// case directory: the closed-dump check counts files, not log lines.
fn dump_files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read case dir {dir:?}: {error}"))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("pilotdeck_request_"))
        .collect();
    names.sort();
    names
}

fn run_case(protocol: &str, enabled: Option<&str>) {
    let marker = format!(
        "dump-marker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = CaseRoot::acquire("run");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "dump_child", "--ignored", "--nocapture"])
        .env("PDRS_DUMP_PROTOCOL", protocol)
        .env("PDRS_DUMP_MARKER", &marker)
        .env("PDRS_DUMP_MODE", "ok")
        // The child's temp dir is this case's exclusive scratch directory.
        .env("TMPDIR", &*root)
        .env_remove("PILOTDECK_DUMP_REQUEST");
    if let Some(value) = enabled {
        command.env("PILOTDECK_DUMP_REQUEST", value);
    }
    let output = command.output().expect("run isolated dump case");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{protocol}/{enabled:?}:\n{stdout}\n{stderr}"
    );
    let sent: Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("SENT_BODY="))
            .expect("child must exercise the transport"),
    )
    .unwrap();
    assert_eq!(sent["dumpMarker"], marker);

    // Closed-dump check: the filesystem of the child's own temp directory,
    // independent of anything the child printed.
    let files = dump_files_in(&root);
    let dumps = reported_dump_paths(&stdout);
    if enabled == Some("1") {
        assert_eq!(
            files.len(),
            1,
            "{protocol}/{enabled:?}: expected exactly one dump file in {:?}: {files:?}",
            &*root
        );
        assert_eq!(
            dumps.len(),
            1,
            "{protocol}/{enabled:?}: expected exactly one dump: {stdout}"
        );
        let path = &dumps[0];
        assert_eq!(
            path.parent(),
            Some(&*root),
            "{protocol}/{enabled:?}: dump escaped the case directory: {path:?}"
        );
        assert_eq!(path.file_name().unwrap().to_string_lossy(), files[0]);
        let contents = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read child-reported dump {path:?}: {error}"));
        assert_eq!(serde_json::from_str::<Value>(&contents).unwrap(), sent);
        assert_eq!(contents, serde_json::to_string_pretty(&sent).unwrap());
    } else {
        assert!(
            dumps.is_empty(),
            "{protocol}/{enabled:?} wrote a dump: {dumps:?}"
        );
        assert!(
            files.is_empty(),
            "{protocol}/{enabled:?} left dump files: {files:?}"
        );
        assert!(!stdout.contains("[model-debug]"), "{stdout}");
    }
    assert!(!stderr.contains("[model-debug]"), "{stderr}");
}

#[test]
fn dump_request_creates_file() {
    // The generic send path is shared by all three HTTP protocols, so the
    // positive control covers each of them.
    for protocol in ["openai", "anthropic", "openai-responses", "google"] {
        run_case(protocol, Some("1"));
    }
}

/// TS gates strictly on `=== "1"` (streamModel.ts:195, 394); anything else —
/// including "0" and "true" — must leave the dump path cold.
#[test]
fn dump_request_disabled_by_default() {
    for protocol in ["openai", "google"] {
        for value in [None, Some("0"), Some("true")] {
            run_case(protocol, value);
        }
    }
}

/// EACCES fixture: a directory this run created and then closed for writing.
/// Kept as a guard so the case cannot end — not even on an assertion-panic
/// unwind — with the directory still at 0o000: drop restores 0o700 first and
/// only then removes it, so nothing leaks under the case root.
struct NoAccessDir(std::path::PathBuf);

/// This run's effective uid. The old probe — listing the freshly closed
/// 0o000 directory to decide whether this run is privileged — also reads as
/// "privileged" in environments that do not enforce DAC (rootless user
/// namespaces, some network filesystems), silently skipping the EACCES case
/// as a false pass; the euid is authoritative for whether the child's open
/// can be expected to fail.
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

impl NoAccessDir {
    /// `None` only when this run's euid is 0: DAC does not apply, so the
    /// child's open would succeed and EACCES cannot be provoked. A non-root
    /// run whose freshly closed directory is still listable means DAC is not
    /// enforced here — an EACCES assertion could not prove anything — so the
    /// case fails loudly instead of being skipped.
    fn create(root: &std::path::Path) -> Option<Self> {
        let dir = root.join("noaccess-tmpdir");
        std::fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
            if effective_uid() == 0 {
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                return None;
            }
            if std::fs::read_dir(&dir).is_ok() {
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                panic!(
                    "euid {} is not 0 yet can list a freshly 0o000 directory: DAC is not \
                     enforced in this environment, so the EACCES dump case cannot be \
                     exercised honestly; failing instead of silently skipping",
                    effective_uid()
                );
            }
        }
        Some(Self(dir))
    }
}

impl Drop for NoAccessDir {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How the child's temp directory is broken, and what the exclusive create
/// reports for it. These are the three write-failure shapes TS was probed
/// with (`_tmp/20261025-issue-621-fix-r2/ts-probe/`); the errno numbers are
/// the same on darwin and linux. The EACCES shape yields `None` under euid 0
/// (DAC does not apply there), so the caller can skip just that shape; the
/// returned guard, when `Some`, must outlive the child run it breaks.
fn broken_tmpdir(
    root: &std::path::Path,
    kind: &str,
) -> Option<(std::path::PathBuf, Option<NoAccessDir>)> {
    match kind {
        // ENOENT: the directory does not exist.
        "enoent" => Some((root.join("nonexistent-tmpdir"), None)),
        // EACCES: the closed-for-writing directory guarded above.
        "eacces" => NoAccessDir::create(root).map(|guard| (guard.0.clone(), Some(guard))),
        // ENOTDIR: a path component is a regular file.
        _ => {
            std::fs::write(root.join("blocker-file"), "not a directory").unwrap();
            Some((root.join("blocker-file").join("sub"), None))
        }
    }
}

/// A failed dump write surfaces like TS (`streamModel.ts:196-203,395-401`,
/// probe evidence in `_tmp/20261025-issue-621-fix-r2/ts-probe/`): the child
/// runs with a temp directory that fails the exclusive create with ENOENT,
/// EACCES, or ENOTDIR.
///
/// Google: the SDK send never happens — zero transport calls — and the
/// attempt yields one non-retryable `error` event whose `code`, `message`,
/// and `raw` (libuv-signed `errno`, `code`, `syscall`, `path` for open-stage
/// failures) match the TS probe. OpenAI: this port's pre-existing (already in
/// main) swallow-and-continue behavior — the send proceeds and only stderr
/// mentions the failure; registered in A6.4-MODEL-DUMP-SECURE-001.
#[test]
fn dump_request_write_failure_semantics() {
    for (kind, code, errno, description) in [
        ("enoent", "ENOENT", "-2", "no such file or directory"),
        ("eacces", "EACCES", "-13", "permission denied"),
        ("enotdir", "ENOTDIR", "-20", "not a directory"),
    ] {
        for protocol in ["google", "openai"] {
            let marker = format!(
                "dump-marker-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let root = CaseRoot::acquire("write-fail");
            // `_noaccess` keeps the EACCES fixture closed for the whole child
            // run and reopens it on drop; declared after `root` so it also
            // drops first.
            let Some((broken, _noaccess)) = broken_tmpdir(&root, kind) else {
                eprintln!("{protocol}/{kind} write-fail: skipped, euid is 0 (DAC does not apply)");
                continue;
            };
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "dump_child", "--ignored", "--nocapture"])
                .env("PDRS_DUMP_PROTOCOL", protocol)
                .env("PDRS_DUMP_MARKER", &marker)
                .env("PDRS_DUMP_MODE", "write-fail")
                .env("PILOTDECK_DUMP_REQUEST", "1")
                .env("TMPDIR", &broken)
                .output()
                .expect("run isolated dump write-failure case");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(
                output.status.success(),
                "{protocol}/{kind} write-fail:\n{stdout}\n{stderr}"
            );
            assert!(
                dump_files_in(&root).is_empty(),
                "{protocol}/{kind} write-fail must not write any dump file"
            );
            match protocol {
                "google" => {
                    assert!(
                        !stdout.contains("SENT_BODY="),
                        "{protocol}/{kind}: Google send must not run after a failed dump: {stdout}"
                    );
                    let line = |prefix: &str| {
                        stdout
                            .lines()
                            .find_map(|line| line.strip_prefix(prefix))
                            .unwrap_or_else(|| {
                                panic!("{protocol}/{kind}: no {prefix} line: {stdout}")
                            })
                    };
                    let expect_line = |prefix: &str, expected: &str, what: &str| {
                        assert_eq!(line(prefix), expected, "{protocol}/{kind}: {what}");
                    };
                    expect_line("ERROR_CODE=", code, "error code");
                    expect_line("ERROR_RETRYABLE=", "false", "retryable");
                    expect_line("ERROR_RAW_ERRNO=", errno, "raw errno (libuv sign)");
                    expect_line("ERROR_RAW_CODE=", code, "raw code");
                    expect_line("ERROR_RAW_SYSCALL=", "open", "raw syscall");
                    // `raw` is the fs Error's own projection, so the path is
                    // the dump candidate under the broken TMPDIR.
                    let raw_path = line("ERROR_RAW_PATH=");
                    let prefix = broken.to_string_lossy();
                    assert!(
                        raw_path.starts_with(prefix.as_ref()),
                        "{protocol}/{kind}: raw path under the broken TMPDIR: {raw_path}"
                    );
                    assert!(
                        raw_path.contains("pilotdeck_request_"),
                        "{protocol}/{kind}: raw path names the dump: {raw_path}"
                    );
                    let message = line("ERROR_MESSAGE=");
                    assert_eq!(
                        message,
                        &format!("{code}: {description}, open '{raw_path}'"),
                        "{protocol}/{kind}: Node writeFileSync message shape"
                    );
                    assert!(
                        !stderr.contains("[model-debug]"),
                        "{protocol}/{kind}: Google path must surface, not swallow, the failure: {stderr}"
                    );
                }
                "openai" => {
                    assert!(
                        stdout.contains("SENT_BODY="),
                        "{protocol}/{kind}: OpenAI pre-existing behavior sends on: {stdout}"
                    );
                    assert!(
                        stderr.contains("[model-debug] Failed to write request dump"),
                        "{protocol}/{kind}: failure is still visible on stderr: {stderr}"
                    );
                }
                _ => unreachable!(),
            }
        }
    }
}

/// A dump write past `ulimit -f` (RLIMIT_FSIZE) must surface like TS, not
/// kill the child: Node ignores SIGXFSZ process-wide, so its `writeFileSync`
/// observes `EFBIG` (`errno -27`, syscall `write`, no path) and the attempt
/// yields one non-retryable error event. Rust std ignores only SIGPIPE, so
/// without a handler the child dies with SIGXFSZ (exit 153) before any
/// io::Error exists (probe: `_tmp/20261025-issue-621-takeover/xfsz-probe/`,
/// rustc 1.99.0 vs node v22.22.0, `ulimit -f 1`, 4 KiB write); the dump path
/// registers a tokio SIGXFSZ handler before writing, which is what this case
/// pins.
#[test]
fn dump_request_write_over_file_size_limit_surfaces_efbig() {
    let marker = format!(
        "dump-marker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = CaseRoot::acquire("xfsz");
    // `ulimit -f 1` caps files at one block (1 KiB on this darwin, 512 B on
    // linux): the exclusive create still succeeds, the write of the (much
    // larger) dump does not.
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg("ulimit -f 1; exec \"$PDRS_CHILD_EXE\" --exact dump_child --ignored --nocapture")
        .env("PDRS_CHILD_EXE", std::env::current_exe().unwrap())
        .env("PDRS_DUMP_PROTOCOL", "google")
        .env("PDRS_DUMP_MARKER", &marker)
        .env("PDRS_DUMP_MODE", "write-fail")
        .env("PILOTDECK_DUMP_REQUEST", "1")
        .env("TMPDIR", &*root)
        .output()
        .expect("run isolated over-limit dump case");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    // Not killed by SIGXFSZ: a signal death has no exit code at all.
    assert_eq!(
        output.status.code(),
        Some(0),
        "google/xfsz write-fail (a SIGXFSZ death reports 153 or no code):\n{stdout}\n{stderr}"
    );
    // A signal death alone cannot tell "handler registered" from "the
    // registration failed and degraded": only the degradation note can, so
    // its absence is what pins the registration actually happened here.
    assert!(
        !stderr.contains("SIGXFSZ handler not registered"),
        "google/xfsz: SIGXFSZ registration must not have degraded:\n{stderr}"
    );
    assert!(
        !stdout.contains("SENT_BODY="),
        "google/xfsz: the SDK send must not run after a failed dump: {stdout}"
    );
    let line = |prefix: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap_or_else(|| panic!("google/xfsz: no {prefix} line: {stdout}"))
    };
    assert_eq!(line("ERROR_CODE="), "EFBIG", "error code");
    assert_eq!(line("ERROR_RETRYABLE="), "false", "retryable");
    assert_eq!(line("ERROR_RAW_ERRNO="), "-27", "raw errno (libuv sign)");
    assert_eq!(line("ERROR_RAW_CODE="), "EFBIG", "raw code");
    assert_eq!(line("ERROR_RAW_SYSCALL="), "write", "raw syscall");
    assert_eq!(
        line("ERROR_RAW_PATH="),
        "MISSING",
        "write-stage failures carry no path, like the TS probe"
    );
    assert_eq!(
        line("ERROR_MESSAGE="),
        "EFBIG: file too large, write",
        "Node write-stage message shape"
    );
    // The partial file stays behind, like TS's `writeFileSync` under the same
    // limit (probe: block-sized remnants from both runtimes).
    let files = dump_files_in(&root);
    assert_eq!(files.len(), 1, "exactly the truncated dump: {files:?}");
    let partial = root.join(&files[0]);
    let size = std::fs::metadata(&partial)
        .unwrap_or_else(|error| panic!("stat partial dump {partial:?}: {error}"))
        .len();
    assert!(
        size > 0 && size <= 1024,
        "partial dump written up to the one-block ulimit, got {size} bytes"
    );
}

/// An empty TMPDIR is skipped the way JS `||` skips it: Node's `os.tmpdir()`
/// falls through to TMP, then TEMP, else `/tmp` (probe:
/// `_tmp/20261025-issue-621-fix-r4/node-probe/`), while
/// `std::env::temp_dir()` answers `""` for a set-but-empty TMPDIR and would
/// drop the dump into the process's working directory. The child runs with
/// its own exclusive working directory, so a cwd-relative dump cannot hide.
#[test]
fn dump_request_empty_tmpdir_never_lands_in_cwd() {
    let marker = format!(
        "dump-marker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = CaseRoot::acquire("empty-tmpdir");
    let cwd = CaseRoot::acquire("empty-tmpdir-cwd");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "dump_child", "--ignored", "--nocapture"])
        .env("PDRS_DUMP_PROTOCOL", "openai")
        .env("PDRS_DUMP_MARKER", &marker)
        .env("PDRS_DUMP_MODE", "ok")
        .env("PILOTDECK_DUMP_REQUEST", "1")
        .env("TMPDIR", "")
        .env("TMP", &*root)
        .env_remove("TEMP")
        .current_dir(&*cwd)
        .output()
        .expect("run isolated empty-TMPDIR dump case");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "empty-tmpdir:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("SENT_BODY="),
        "empty-tmpdir: the send must still run: {stdout}"
    );
    assert!(
        dump_files_in(&cwd).is_empty(),
        "empty TMPDIR must never drop the dump into the working directory: {:?}",
        &*cwd
    );
    let dumps = reported_dump_paths(&stdout);
    assert_eq!(dumps.len(), 1, "expected exactly one dump: {stdout}");
    assert_eq!(
        dumps[0].parent(),
        Some(&*root),
        "empty TMPDIR must fall through to TMP (node os.tmpdir()): {:?}",
        dumps[0]
    );
    assert_eq!(
        dump_files_in(&root).len(),
        1,
        "the dump lands in the TMP directory, not the cwd"
    );
}

/// TS `complete()` never dumps: `streamModel.ts:124` calls
/// `sendProviderRequest` directly, outside the stream loop's dump gate at
/// `:196`. The Rust send path is shared by `complete_model` and
/// `stream_model`, so this pins that the non-streaming call stays cold even
/// with `PILOTDECK_DUMP_REQUEST=1`.
#[test]
fn dump_request_not_triggered_by_complete() {
    let marker = format!(
        "dump-marker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = CaseRoot::acquire("complete");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "dump_child", "--ignored", "--nocapture"])
        .env("PDRS_DUMP_PROTOCOL", "openai")
        .env("PDRS_DUMP_MARKER", &marker)
        .env("PDRS_DUMP_MODE", "complete")
        .env("PILOTDECK_DUMP_REQUEST", "1")
        .env("TMPDIR", &*root)
        .output()
        .expect("run isolated complete dump case");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "complete:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("SENT_BODY="),
        "complete() must still send: {stdout}"
    );
    assert!(
        !stdout.contains("[model-debug]"),
        "complete() must not dump: {stdout}"
    );
    assert!(
        !stderr.contains("[model-debug]"),
        "complete() must not log a dump: {stderr}"
    );
    assert!(
        dump_files_in(&root).is_empty(),
        "complete() must not write any dump file: {:?}",
        &*root
    );
}

#[tokio::test]
#[ignore = "subprocess helper; run through the parent tests"]
async fn dump_child() {
    let protocol = std::env::var("PDRS_DUMP_PROTOCOL").unwrap();
    let marker = std::env::var("PDRS_DUMP_MARKER").unwrap();
    let mode = std::env::var("PDRS_DUMP_MODE").unwrap_or_else(|_| "ok".into());
    let request = parse_json(
        &json!({
            "provider": "dump-provider", "model": "dump-model",
            // The body must clear one `ulimit -f` block (1 KiB on this
            // darwin, 512 B on linux) so the over-limit case fails inside
            // the write, not before it.
            "messages": [{"role": "user", "content": [{"type": "text", "text":
                format!("dump test {}", "x".repeat(4096))}]}]
        })
        .to_string(),
    )
    .unwrap();
    let config = parse_json(
        &json!({"providers": {"dump-provider": {
            "id": "dump-provider", "protocol": protocol,
            "url": "http://dump.invalid/v1", "apiKey": "test-only",
            "retry": {"streamMaxRetries": 0, "requestMaxRetries": 0},
            "extraBody": {"dumpMarker": marker},
            "models": {"dump-model": {
                "id": "dump-model", "capabilities": {"supportsStreaming": true},
                "multimodal": {"input": ["text"]}
            }}
        }}})
        .to_string(),
    )
    .unwrap();
    let transport = CaptureTransport::default();
    let mut error_event: Option<pilotdeck_protocol::JsValue> = None;
    let mut collect_error = |output: StreamOutput| {
        if let StreamOutput::Event(event) = output
            && event
                .get("type")
                .and_then(|value| value.as_string())
                .is_some_and(|kind| kind.equals_str("error"))
        {
            error_event = Some(event.clone());
        }
    };
    if mode == "complete" {
        // TS `complete()` never dumps; the shared send path must stay cold.
        let result = complete_model(&transport, &request, &config, &StreamOptions::default()).await;
        let bodies = transport.0.lock().unwrap();
        assert!(
            result.is_err(),
            "transport failure must propagate: {result:?}"
        );
        assert_eq!(
            bodies.len(),
            1,
            "complete() must reach the provider send exactly once: {result:?}"
        );
        assert_eq!(bodies[0]["dumpMarker"], marker);
        println!("SENT_BODY={}", bodies[0]);
        return;
    }
    let result = stream_model(
        &transport,
        &request,
        &config,
        &StreamOptions::default(),
        &mut collect_error,
    )
    .await;
    let bodies = transport.0.lock().unwrap();
    if mode == "write-fail" {
        // With a dead temp directory: Google never sends and yields one
        // error event; OpenAI (pre-existing port behavior) still sends.
        if protocol == "google" {
            assert!(result.is_ok(), "error event path completes: {result:?}");
            assert!(
                bodies.is_empty(),
                "Google SDK send must not run after a failed dump: {bodies:?}"
            );
            let error = error_event
                .as_ref()
                .and_then(|event| event.get("error"))
                .expect("Google yields an error event");
            let text = |field: &str| {
                error
                    .get(field)
                    .and_then(|value| value.as_string())
                    .and_then(|s| s.to_utf8().ok())
                    .unwrap_or_default()
            };
            println!("ERROR_CODE={}", text("code"));
            println!(
                "ERROR_RETRYABLE={}",
                if error
                    .get("retryable")
                    .and_then(pilotdeck_protocol::JsValue::as_bool)
                    == Some(true)
                {
                    "true"
                } else {
                    "false"
                }
            );
            println!("ERROR_MESSAGE={}", text("message"));
            // The fs Error's own projection TS keeps as `raw`: print each
            // field, or `MISSING` when absent, so the parent can pin presence
            // and value separately.
            let raw = error.get("raw");
            let raw_field = |name: &str| raw.and_then(|raw| raw.get(name));
            match raw_field("errno") {
                Some(pilotdeck_protocol::JsValue::Number(value)) => {
                    println!("ERROR_RAW_ERRNO={value}");
                }
                _ => println!("ERROR_RAW_ERRNO=MISSING"),
            }
            let raw_text = |name: &str| {
                raw_field(name)
                    .and_then(pilotdeck_protocol::JsValue::as_string)
                    .and_then(|value| value.to_utf8().ok())
                    .unwrap_or_else(|| "MISSING".to_string())
            };
            println!("ERROR_RAW_CODE={}", raw_text("code"));
            println!("ERROR_RAW_SYSCALL={}", raw_text("syscall"));
            println!("ERROR_RAW_PATH={}", raw_text("path"));
        } else {
            assert_eq!(
                bodies.len(),
                1,
                "OpenAI pre-existing behavior still sends: {result:?}"
            );
            println!("SENT_BODY={}", bodies[0]);
        }
        return;
    }
    assert!(
        result.is_err() || error_event.is_some(),
        "transport failure must propagate as an error or a Google error event"
    );
    assert_eq!(
        bodies.len(),
        1,
        "must reach the provider send path once: {result:?}"
    );
    assert_eq!(bodies[0]["dumpMarker"], marker);
    println!("SENT_BODY={}", bodies[0]);
}
