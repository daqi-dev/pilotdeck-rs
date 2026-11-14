use super::*;
use crate::host::ModuleError;
use crate::runtime::{
    model::router::{AbortFlag, RouterFuture, RouterModelInvocation},
    project::FamilyResource,
    turn::runner::tail::TitleInput,
};
use crate::{negative_assert, negative_assert_eq};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
};

fn oracle() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/native-process/oracle.json"
    ))
    .expect("#1071 oracle JSON")
}
fn js(value: &Value) -> JsValue {
    pilotdeck_protocol::parse_json(&value.to_string()).expect("#1071 test JSON")
}
fn json_value(value: &JsValue) -> Value {
    serde_json::from_str(
        &pilotdeck_protocol::stringify_json(value)
            .expect("#1071 stringify")
            .expect("#1071 serializable value"),
    )
    .expect("#1071 serialized JSON")
}
struct Scratch(PathBuf);
impl Scratch {
    // #2335: guard first — a panic inside the build loop otherwise leaks
    // the temp tree; only a constructed `Scratch` unwinds through `Drop`.
    // The canonical path is written back into the guard, never a second
    // `Self(...)`, which would drop-and-delete the tree it points at.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-1071-{}", uuid::Uuid::new_v4()));
        let mut scratch = Self(root);
        for name in ["home", "project"] {
            std::fs::create_dir_all(scratch.0.join(name)).expect("#1071 scratch dirs");
        }
        for name in ["token", "sidecar"] {
            std::fs::write(scratch.0.join(name), "fixture").expect("#1071 scratch file");
        }
        let canonical = scratch
            .0
            .canonicalize()
            .expect("#1071 scratch canonical path");
        scratch.0 = canonical;
        scratch
    }
    fn args(&self) -> Vec<OsString> {
        [
            "--home",
            self.0.join("home").to_str().expect("#1071 home UTF8"),
            "--project",
            self.0.join("project").to_str().expect("#1071 project UTF8"),
            "--listen",
            "127.0.0.1:0",
            "--token-file",
            self.0.join("token").to_str().expect("#1071 token UTF8"),
            "--sidecar",
            self.0.join("sidecar").to_str().expect("#1071 sidecar UTF8"),
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn explicit_options_and_environment() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let captured = std::panic::catch_unwind(|| {
            capture_environment([(OsString::from("NON_UTF8"), OsString::from_vec(vec![255]))])
        });
        negative_assert!(
            captured.is_ok(),
            "#1071 non UTF-8 environment does not panic"
        );
        negative_assert_eq!(
            captured
                .expect("#1071 captured environment was checked")
                .get("NON_UTF8"),
            Some(&"\u{fffd}".to_owned()),
            "#1071 non UTF-8 environment replacement preserved"
        );
    }
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1071 valid isolated options");
    negative_assert!(
        native_requested(Some(std::ffi::OsStr::new("--native"))),
        "#1071 explicit native switch"
    );
    for first in [
        None,
        Some(std::ffi::OsStr::new("native")),
        Some(std::ffi::OsStr::new("--native=true")),
        Some(std::ffi::OsStr::new("--help")),
    ] {
        negative_assert!(!native_requested(first), "#1071 A4 remains default");
    }
    for (key, bad) in [
        ("--home", "relative"),
        ("--listen", "0.0.0.0:0"),
        ("--listen", "127.0.0.1:80"),
        ("--drain-ms", "0"),
        ("--drain-ms", "300001"),
        ("--unknown", "x"),
    ] {
        let mut args = scratch.args();
        if let Some(index) = args.iter().position(|v| v == key) {
            args[index + 1] = bad.into();
        } else {
            args.extend([key.into(), bad.into()]);
        }
        negative_assert!(
            Options::parse(args).is_err(),
            "#1071 invalid configuration rejected"
        );
    }
    let mut same = scratch.args();
    same[3] = same[1].clone();
    negative_assert!(Options::parse(same).is_err(), "#1071 separated roots");
    let mut duplicate = scratch.args();
    duplicate.extend(["--home".into(), scratch.0.join("home").into_os_string()]);
    negative_assert!(
        Options::parse(duplicate).is_err(),
        "#1071 duplicate option rejected"
    );
    negative_assert!(
        Options::parse(Vec::<OsString>::new()).is_err(),
        "#1071 required authority"
    );
    let inherited = Env::from_iter([
        ("HOME".into(), "outside".into()),
        ("PILOT_HOME".into(), "outside".into()),
        ("PILOTDECK_CONFIG_PATH".into(), "outside.yaml".into()),
        ("PILOTDECK_PROXY_UPSTREAM".into(), "outside".into()),
        ("PILOTDECK_AGENT_LOOP_TCP_HOST".into(), "outside".into()),
        ("PILOTDECK_AGENT_LOOP_TCP_PORT".into(), "99".into()),
        // #1431 P2-2: without `--storage-root` both snapshot roots are ambient
        // state; the isolated process must drop them, not inherit them.
        ("PILOTDECK_STORAGE_ROOT".into(), "outside".into()),
        ("PILOTDECK_SNAPSHOT_ROOT".into(), "outside".into()),
        // #1354: the mode item is entry scope — this process already
        // consumed it for its own routing.
        ("PILOTDECK_GATEWAY_MODE".into(), "native".into()),
    ]);
    let env = options.environment(inherited);
    negative_assert_eq!(
        env.get("HOME"),
        options.home.to_str().map(|v| v.to_owned()).as_ref(),
        "#1071 isolated HOME"
    );
    negative_assert_eq!(
        env.get("PILOT_HOME"),
        env.get("HOME"),
        "#1071 isolated PILOT_HOME"
    );
    for key in [
        "PILOTDECK_CONFIG_PATH",
        "PILOTDECK_PROXY_UPSTREAM",
        "PILOTDECK_AGENT_LOOP_TCP_HOST",
        "PILOTDECK_AGENT_LOOP_TCP_PORT",
        // #1431 P2-2: the None branch of `Options::environment` drops both
        // snapshot roots; a mutant that keeps either must die here.
        "PILOTDECK_STORAGE_ROOT",
        "PILOTDECK_SNAPSHOT_ROOT",
    ] {
        negative_assert!(!env.contains_key(key), "#1071 inherited authority removed");
    }
    // #1354 entry-scope mode item not inherited: the routing switch must not
    // sink into the native subtree's environment — a descendant re-executing
    // the gateway would otherwise silently inherit a native routing.
    negative_assert!(
        !env.contains_key("PILOTDECK_GATEWAY_MODE"),
        "#1354 entry-scope mode item not inherited"
    );
    for key in [
        "PILOTDECK_MANAGED_CONFIG",
        "PILOTDECK_STORAGE_ROOT",
        "PILOTDECK_SNAPSHOT_ROOT",
    ] {
        negative_assert!(
            validate_environment(&Env::from_iter([(key.into(), "outside".into())])).is_err(),
            "#1071 ambient state refused"
        );
    }
    negative_assert!(
        validate_environment(&Env::new()).is_ok(),
        "#1071 clean environment accepted"
    );
    // #1431 P2-2: an empty inherited value is still removed from the captured
    // environment (the isolated process never reads ambient snapshot roots).
    let blank = options.environment(Env::from_iter([
        ("PILOTDECK_STORAGE_ROOT".into(), String::new()),
        ("PILOTDECK_SNAPSHOT_ROOT".into(), String::new()),
    ]));
    negative_assert!(
        !blank.contains_key("PILOTDECK_STORAGE_ROOT")
            && !blank.contains_key("PILOTDECK_SNAPSHOT_ROOT"),
        "#1431 blank inherited snapshot roots removed"
    );
    // #1431 P2-2: the Some branch injects the CLI root and never leaks the
    // ambient storage value; a separate snapshot-root override is preserved
    // for `resolve_workspace_snapshot_config`. This pins `Options::environment`
    // itself: the entry (`native_process::run`) rejects any inherited non-empty
    // managed/storage/snapshot root *before* calling it, so a non-empty
    // inherited snapshot root only reaches this branch in a direct call. The
    // reachable-from-the-entry case (empty inherited value) is the `blank`
    // block above.
    let storage = scratch.0.join("storage");
    std::fs::create_dir_all(&storage).expect("#1431 storage root directory");
    let mut with_storage = scratch.args();
    with_storage.extend([
        OsString::from("--storage-root"),
        storage.clone().into_os_string(),
    ]);
    let options = Options::parse(with_storage).expect("#1431 storage root options");
    let env = options.environment(Env::from_iter([
        ("PILOTDECK_STORAGE_ROOT".into(), "inherited".into()),
        (
            "PILOTDECK_SNAPSHOT_ROOT".into(),
            "inherited-snapshots".into(),
        ),
    ]));
    negative_assert_eq!(
        env.get("PILOTDECK_STORAGE_ROOT").map(String::as_str),
        storage.to_str(),
        "#1431 storage root injected from --storage-root"
    );
    negative_assert_eq!(
        env.get("PILOTDECK_SNAPSHOT_ROOT").map(String::as_str),
        Some("inherited-snapshots"),
        "#1431 snapshot root override preserved"
    );
}

/// #1431 P2-3: a non-UTF-8 `--project` is refused at parse.
///
/// Measured (2026-11-07, cb8): the turn side computes its workspace id from
/// `options.project.to_string_lossy()` (`metadata` is lossy end to end, like
/// Node's string-only filesystem API), while `serves_project` compares the raw
/// `PathBuf` bytes. A JS client can only ever send the lossy spelling, so that
/// spelling is refused by every management request — the id such a turn
/// captured under could never be listed. Refusing the unrepresentable path at
/// parse (the `--storage-root` precedent) keeps the two sides identical
/// instead of silently capturing into an unreachable workspace.
///
/// The path is deliberately never created: the rejection runs before any
/// filesystem access (`resolve_process_path` is lexical), and APFS/HFS+ cannot
/// hold non-UTF-8 names at all (`create_dir_all` → `EILSEQ`, os error 92), so a
/// fixture that needed one would fail on macOS before reaching the assertion.
#[cfg(unix)]
#[test]
fn non_utf8_project_path_is_refused() {
    use std::os::unix::ffi::OsStringExt;
    let scratch = Scratch::new();
    let raw = scratch
        .0
        .join(std::ffi::OsString::from_vec(b"project-\xff".to_vec()));
    let mut args = scratch.args();
    args[3] = raw.clone().into_os_string();
    let error = Options::parse(args).err().map(|error| error.to_string());
    negative_assert_eq!(
        error.as_deref(),
        Some("project is not UTF-8"),
        "#1431 non-UTF-8 --project must be rejected"
    );
    // Control: the same arguments with the UTF-8 project parse, so the
    // rejection above is about the bytes and not an unrelated option.
    Options::parse(scratch.args()).expect("#1431 UTF-8 project control parses");
    // Measurement behind the refusal: with the raw project configured, the one
    // spelling a JS client can send (the lossy one) is not served, so no
    // `snapshot_list` for this workspace is reachable.
    let mut parsed = Options::parse(scratch.args()).expect("#1431 control options");
    parsed.project = raw;
    negative_assert!(
        !parsed.serves_project(&parsed.project.to_string_lossy()),
        "#1431 the lossy project spelling a client must send is not served"
    );
}

/// #2036: the triage contract behind the probe, in one place. Only `EILSEQ` —
/// what APFS/HFS+ return for these bytes, measured on darwin 25.6 for
/// `open(O_CREAT)`/`mkdir`/`create_dir_all` alike (os error 92) — means "this
/// filesystem can never host the case". Everything else is our scratch's own
/// failure and must fail closed: folding another errno into the skip would
/// silently delete the only coverage of the raw `--home` / `--sidecar`
/// rejection on the one platform that can run it.
///
/// #1672 (issues A + B, same fix as the cron twin
/// `runtime::cron::tests::non_utf8_names_writable`): the decision is a
/// tested unit, not an untested call-site `match` — `triage_probe` maps
/// every outcome (including the fd limit, EMFILE/ENFILE, with exactly one
/// bounded retry and its own marker; see the cron twin's doc for the mbp2
/// measurement) and `non_utf8_probe_skip` folds it into skip/fail-closed,
/// both executed by the unit tests below on every host with synthetic
/// errors.
#[cfg(unix)]
#[derive(Debug)]
enum NonUtf8Probe {
    /// The scratch accepted the raw bytes: the case can run here.
    Writable,
    /// The name bytes themselves are rejected: skip, never fail.
    RejectsName,
    /// The process or system fd limit (EMFILE/ENFILE): the failure's cause
    /// lives outside the probed directory.
    FdLimit(std::io::Error),
    /// Any other errno: a real failure of the scratch, must fail closed.
    Failed(std::io::Error),
}

#[cfg(unix)]
fn triage_probe(mut attempt: impl FnMut() -> std::io::Result<()>) -> NonUtf8Probe {
    let mut outcome = attempt();
    if let Err(error) = &outcome
        && probe_hit_the_fd_limit(error)
    {
        outcome = attempt();
    }
    match outcome {
        Ok(()) => NonUtf8Probe::Writable,
        Err(error) if error.raw_os_error() == Some(libc::EILSEQ) => NonUtf8Probe::RejectsName,
        Err(error) if probe_hit_the_fd_limit(&error) => NonUtf8Probe::FdLimit(error),
        Err(error) => NonUtf8Probe::Failed(error),
    }
}

/// #1672 (issue B): whether this probe failure is the process's own fd
/// table (EMFILE) or the system's open-file table (ENFILE) running out —
/// the one errno family whose cause lives outside the probed directory.
#[cfg(unix)]
fn probe_hit_the_fd_limit(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EMFILE) | Some(libc::ENFILE)
    )
}

/// #1672 (issue A): the skip decision over a triaged outcome — the half
/// that used to sit untested at the call site. A regression to "any error
/// skips" silently deletes the raw `--home` / `--sidecar` coverage on the
/// one platform that can run it; the unit test below executes every arm,
/// so that regression is a red test, not a code review.
#[cfg(unix)]
fn non_utf8_probe_skip(probe: &NonUtf8Probe, probed: &Path) -> bool {
    match probe {
        NonUtf8Probe::Writable => true,
        NonUtf8Probe::RejectsName => false,
        NonUtf8Probe::FdLimit(error) => panic!(
            "#2036 probing {} hit the process fd limit, not the file name: {error}",
            probed.display()
        ),
        NonUtf8Probe::Failed(error) => panic!(
            "#2036 probing {} failed for a reason other than the file name: {error}",
            probed.display()
        ),
    }
}

/// The executable half of that contract. Runs on every host: the errors are
/// synthetic, so it does not need a filesystem that can refuse a raw name.
#[cfg(unix)]
#[test]
fn triage_probe_sorts_every_outcome_with_one_bounded_fd_retry() {
    let error = |errno: i32| std::io::Error::from_raw_os_error(errno);
    assert!(
        matches!(triage_probe(|| Ok(())), NonUtf8Probe::Writable),
        "#2036 a succeeding probe means the scratch accepts the raw bytes: run the case, do not skip"
    );
    // fix-r1 (DS boundary P2): the counting closures pin the tight half of
    // the retry contract too — a non-fd error is attempted exactly once, so
    // loosening the retry condition into "any error retries" is a red test.
    let mut calls = 0;
    assert!(
        matches!(
            triage_probe(|| {
                calls += 1;
                Err(error(libc::EILSEQ))
            }),
            NonUtf8Probe::RejectsName
        ) && calls == 1,
        "#2036 EILSEQ means the filesystem rejects these bytes: skip, do not fail, with no retry"
    );
    for errno in [
        libc::EACCES,
        libc::ENOSPC,
        libc::EROFS,
        libc::EINVAL,
        libc::ENOENT,
    ] {
        let mut calls = 0;
        assert!(
            matches!(
                triage_probe(|| {
                    calls += 1;
                    Err(error(errno))
                }),
                NonUtf8Probe::Failed(_)
            ) && calls == 1,
            "#2036 errno {errno} is a real failure of the scratch, not a rejecting filesystem: it must fail closed, not skip, and must not be retried"
        );
    }
    // #1672 (issue B): the fd limit is its own triage category, not a
    // scratch failure whose panic message would blame the file name for a
    // process-level condition.
    for errno in [libc::EMFILE, libc::ENFILE] {
        let mut calls = 0;
        assert!(
            matches!(
                triage_probe(|| {
                    calls += 1;
                    Err(error(errno))
                }),
                NonUtf8Probe::FdLimit(_)
            ) && calls == 2,
            "#2036 errno {errno} is the process or system fd limit, not the scratch's own failure: it must reach its own retry arm, not the scratch-failure marker, retried exactly once"
        );
    }
    // The retry is exactly one: an fd freed after the first EMFILE lets
    // the second attempt through.
    let mut calls = 0;
    let outcome = triage_probe(|| {
        calls += 1;
        if calls == 1 {
            Err(error(libc::EMFILE))
        } else {
            Ok(())
        }
    });
    assert!(
        matches!(outcome, NonUtf8Probe::Writable) && calls == 2,
        "#2036 an EMFILE probe gets exactly one retry: a later-freed fd must let the second attempt through"
    );
    // Termination: a persistent limit is retried once and then stops —
    // never a loop — landing in the fd-limit category.
    let mut calls = 0;
    let outcome = triage_probe(|| {
        calls += 1;
        Err(error(libc::EMFILE))
    });
    assert!(
        matches!(outcome, NonUtf8Probe::FdLimit(_)) && calls == 2,
        "#2036 the EMFILE retry is bounded: exactly one retry, then the fd-limit outcome, never a loop"
    );
    // The retried attempt's terminal errno wins: an EILSEQ seen after the
    // fd-limit retry still skips — that host genuinely cannot host the
    // case.
    let mut calls = 0;
    let outcome = triage_probe(|| {
        calls += 1;
        if calls == 1 {
            Err(error(libc::EMFILE))
        } else {
            Err(error(libc::EILSEQ))
        }
    });
    assert!(
        matches!(outcome, NonUtf8Probe::RejectsName) && calls == 2,
        "#2036 the retried attempt's errno wins: an EILSEQ seen after the fd-limit retry still skips"
    );
}

/// #1672 (issue A): the fail-closed half — the piece that used to live
/// only at the call site, where no test could reach it. A regression to
/// `Err(_) => false` (any error skips) makes this test fail instead of
/// silently deleting the raw-name coverage.
#[cfg(unix)]
#[test]
fn non_eilseq_probe_outcomes_fail_closed_at_the_skip_decision() {
    let probed = Path::new("/nonexistent-probe");
    assert!(
        non_utf8_probe_skip(&NonUtf8Probe::Writable, probed),
        "#2036 a writable probe must run the case, not skip it"
    );
    assert!(
        !non_utf8_probe_skip(&NonUtf8Probe::RejectsName, probed),
        "#2036 a name-rejecting filesystem must skip the case, not fail it"
    );
    for (probe, marker) in [
        (
            NonUtf8Probe::Failed(std::io::Error::from_raw_os_error(libc::EACCES)),
            "failed for a reason other than the file name",
        ),
        (
            NonUtf8Probe::FdLimit(std::io::Error::from_raw_os_error(libc::EMFILE)),
            "hit the process fd limit",
        ),
    ] {
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            non_utf8_probe_skip(&probe, probed)
        }))
        .expect_err("#2036 a probe failure must fail closed (panic), not skip");
        let text = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            text.contains("#2036") && text.contains(marker),
            "#2036 the fail-closed panic must carry its own diagnosis ({marker}), got: {text}"
        );
    }
}

/// The probe performs the file-write shape; the `--home` case below additionally
/// needs `create_dir_all`. Both shapes were measured to return the same `EILSEQ`
/// on APFS and to succeed on ext4, and any other outcome panics rather than
/// skipping, so the difference cannot soften the guard.
#[cfg(unix)]
fn non_utf8_names_writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStringExt;
    let probe = dir.join(OsString::from_vec(b"probe-\xff".to_vec()));
    let outcome = triage_probe(|| std::fs::write(&probe, b""));
    let _ = std::fs::remove_file(&probe);
    non_utf8_probe_skip(&outcome, &probe)
}

/// #2036: a non-UTF-8 `--home` / `--sidecar` is refused at parse, exactly like
/// `--project` (#1431) and `--storage-root` (#1323).
///
/// Unlike `--project`, these two are canonicalised before they enter `Options`,
/// so the case has to exist on disk to reach the rejection; the probe above
/// turns "this filesystem cannot hold raw names" into an explicit skip.
#[cfg(unix)]
#[test]
fn non_utf8_home_and_sidecar_are_refused() {
    use std::os::unix::ffi::OsStringExt;
    let scratch = Scratch::new();
    if !non_utf8_names_writable(&scratch.0) {
        // #2036: the skip is printed, never silent — on APFS a run of this
        // test has to be distinguishable from one that actually covered the
        // rejection. Written through the shared writer, not `eprintln!`
        // (libtest captures the macro for a passing test), and registered in
        // `.config/nextest.toml` so CI replays the line (#1525).
        crate::test_skip_line::stderr_skip_line(
            "non_utf8_home_and_sidecar_are_refused: skipped, \
             filesystem rejects non-UTF-8 file names\n",
        );
        return;
    }
    // Control: the same arguments with UTF-8 spellings parse, so the rejection
    // below is about the bytes and not an unrelated option.
    Options::parse(scratch.args()).expect("#2036 UTF-8 control parses");

    let raw_home = scratch.0.join(OsString::from_vec(b"home-\xff".to_vec()));
    std::fs::create_dir_all(&raw_home).expect("#2036 raw home directory");
    let mut args = scratch.args();
    args[1] = raw_home.clone().into_os_string();
    let error = Options::parse(args).err().map(|error| error.to_string());
    negative_assert_eq!(
        error.as_deref(),
        Some("home is not UTF-8"),
        "#2036 non-UTF-8 --home must be rejected"
    );

    // #2036: a UTF-8 argv spelling can still resolve to raw bytes through a
    // symlink. Judging the value that lands in `Options` (the canonical path)
    // and not the argv text is the whole point of this change, so pin it: the
    // link name is plain ASCII and the rejection still has to happen. A mutant
    // that moved the check before `canonicalize` would keep the two cases above
    // green and only fail here.
    let link_home = scratch.0.join("home-link");
    std::os::unix::fs::symlink(&raw_home, &link_home).expect("#2036 home symlink");
    let mut args = scratch.args();
    args[1] = link_home.into_os_string();
    let error = Options::parse(args).err().map(|error| error.to_string());
    negative_assert_eq!(
        error.as_deref(),
        Some("home is not UTF-8"),
        "#2036 --home resolving through a symlink to raw bytes must be rejected"
    );

    let raw_sidecar = scratch.0.join(OsString::from_vec(b"sidecar-\xff".to_vec()));
    std::fs::write(&raw_sidecar, "fixture").expect("#2036 raw sidecar file");
    let mut args = scratch.args();
    args[9] = raw_sidecar.clone().into_os_string();
    let error = Options::parse(args).err().map(|error| error.to_string());
    negative_assert_eq!(
        error.as_deref(),
        Some("sidecar is not UTF-8"),
        "#2036 non-UTF-8 --sidecar must be rejected"
    );

    let link_sidecar = scratch.0.join("sidecar-link");
    std::os::unix::fs::symlink(&raw_sidecar, &link_sidecar).expect("#2036 sidecar symlink");
    let mut args = scratch.args();
    args[9] = link_sidecar.into_os_string();
    let error = Options::parse(args).err().map(|error| error.to_string());
    negative_assert_eq!(
        error.as_deref(),
        Some("sidecar is not UTF-8"),
        "#2036 --sidecar resolving through a symlink to raw bytes must be rejected"
    );

    // Both fields validated in one parse: the invariant the whole change buys.
    // With the raw spellings refused, every `Options` that exists has a UTF-8
    // home, so `environment()`'s lossy `HOME` is the identity of the raw path
    // children and gateway both use.
    let options = Options::parse(scratch.args()).expect("#2036 UTF-8 options");
    let env = options.environment(Env::new());
    negative_assert_eq!(
        env.get("HOME").map(String::as_str),
        options.home.to_str(),
        "#2036 accepted home is UTF-8, so HOME is the raw path"
    );
}

#[tokio::test]
async fn default_lsp_matches_ts() {
    let service = families::DefaultLsp::default();
    let bound = service.bind().expect("#1071 LSP bind");
    for (name, service, workspace, path, line) in [
        ("empty", &service, "/isolated/project", "file.ts", 0.0),
        ("bound", &bound, "/isolated/project", "file.ts", 0.0),
        ("workspace", &service, "", "file.ts", 0.0),
        ("position", &service, "/isolated/project", "file.ts", -1.0),
        ("escape", &service, "/isolated/project", "../file.ts", 0.0),
        ("dot", &service, "/isolated/project", "dir.ts/.", 0.0),
        ("parent", &service, "/isolated/project", "dir.ts/..", 0.0),
    ] {
        let error = crate::test_negative_receipt::expect_err(
            service.query(workspace, path, line, 0.0),
            "#1071 LSP query refuses unavailable",
        );
        let detail = error.js_error().expect("#1071 LSP typed error");
        let actual = json!({"name":detail.name,"code":detail.code,"message":String::from_utf16_lossy(detail.message.units())});
        let fixture = oracle();
        let expected = fixture["observations"]["lsp"]
            .as_array()
            .expect("#1071 LSP rows")
            .iter()
            .find(|r| r["name"] == name)
            .expect("#1071 LSP case");
        negative_assert_eq!(actual, expected["error"], "#1071 LSP oracle");
    }
    service.dispose().await.expect("#1071 LSP dispose");
    service.dispose().await.expect("#1071 LSP dispose twice");
    negative_assert!(service.bind().is_err(), "#1071 disposed LSP rejects bind");
    negative_assert!(
        bound
            .query("/isolated/project", "file.ts", 0.0, 0.0)
            .expect_err("#1071 bound empty LSP")
            .js_error()
            .expect("#1071 bound typed error")
            .code
            .as_ref()
            .is_some_and(|v| v == "LSP_UNAVAILABLE"),
        "#1071 bound LSP independent lifetime"
    );
}

struct Model {
    response: JsValue,
    calls: Arc<Mutex<Vec<JsValue>>>,
    gate: Option<Arc<tokio::sync::Notify>>,
    entered: Arc<tokio::sync::Notify>,
    abort: Arc<Mutex<Option<AbortFlag>>>,
    reject: bool,
}
impl Default for Model {
    fn default() -> Self {
        Self {
            response: JsValue::Undefined,
            calls: Arc::default(),
            gate: None,
            entered: Arc::default(),
            abort: Arc::default(),
            reject: false,
        }
    }
}
impl RouterModelInvocation for Model {
    fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn get_multimodal(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn complete(&self, request: JsValue, abort: Option<AbortFlag>) -> RouterFuture<JsValue> {
        self.calls
            .lock()
            .expect("#1071 model calls lock")
            .push(request);
        *self.abort.lock().expect("#1071 model abort lock") = abort;
        let gate = self.gate.clone();
        let entered = self.entered.clone();
        let response = self.response.clone();
        let reject = self.reject;
        Box::pin(async move {
            entered.notify_one();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            if reject {
                Err(crate::runtime::model::router::RouterInvokeError::other(
                    "cancelled",
                ))
            } else {
                Ok(response)
            }
        })
    }
}
fn title_input(text: JsString) -> TitleInput {
    TitleInput {
        text,
        session_id: "session".into(),
        turn_id: "turn".into(),
        message_sequences: vec![],
        signal: Default::default(),
        model: None,
    }
}
#[tokio::test]
async fn title_matches_ts_requests_and_results() {
    let fixture = oracle();
    for (name, text, content, override_model) in [
        ("empty", " \n\t ".to_owned(), json!([]), None),
        (
            "normal",
            "  中文\n请求  ".to_owned(),
            json!([{"type":"text","text":"```json\n{\"title\":\"  标题  内容  \"}\n```"}]),
            None,
        ),
        (
            "override",
            "a".repeat(1201),
            json!([{"type":"text","text":json!({"title":"b".repeat(81)}).to_string()}]),
            Some(js(&json!({"provider":"other","model":"chosen"}))),
        ),
        (
            "invalid",
            "request".to_owned(),
            json!([{"type":"text","text":"invalid json"}]),
            None,
        ),
    ] {
        let model = Arc::new(Model {
            response: js(&json!({"content":content})),
            ..Default::default()
        });
        let provider = families::title_provider(
            model.clone(),
            js(&json!({"provider":"fixture","model":"model"})),
            "zh-CN".into(),
            Duration::from_secs(30),
        );
        let mut input = title_input(text.into());
        input.model = override_model;
        let result = (provider.generate)(input)
            .await
            .expect("#1071 title generate");
        let mut calls: Vec<Value> = model
            .calls
            .lock()
            .expect("#1071 calls observed")
            .iter()
            .map(json_value)
            .collect();
        for call in &mut calls {
            let prompt = call
                .as_object_mut()
                .expect("#1071 request object")
                .remove("systemPrompt")
                .expect("#1071 title prompt");
            call["systemPromptSha256"] = json!(format!(
                "{:x}",
                Sha256::digest(prompt.as_str().expect("#1071 prompt string").as_bytes())
            ));
        }
        let expected = fixture["observations"]["title"]
            .as_array()
            .expect("#1071 title rows")
            .iter()
            .find(|r| r["name"] == name)
            .expect("#1071 title row");
        negative_assert_eq!(
            json!(result.map(|s| String::from_utf16_lossy(s.units()))),
            expected["result"],
            "#1071 title result oracle"
        );
        negative_assert_eq!(
            json!(calls),
            expected["calls"],
            "#1071 title request oracle"
        );
    }
}
#[tokio::test(start_paused = true)]
async fn title_waits_for_provider_after_abort() {
    for mode in [
        "cancel-release",
        "timeout-release",
        "cancel-reject",
        "cancel-never",
    ] {
        let gate = Arc::new(tokio::sync::Notify::new());
        let model = Arc::new(Model {
            response: js(&json!({"content":[{"type":"text","text":"{\"title\":\"late title\"}"}]})),
            gate: Some(gate.clone()),
            reject: mode == "cancel-reject",
            ..Default::default()
        });
        let provider = families::title_provider(
            model.clone(),
            JsValue::Undefined,
            "en".into(),
            Duration::from_millis(1),
        );
        let input = title_input("hello".into());
        let cancel = input.signal.clone();
        let task = tokio::spawn((provider.generate)(input));
        tokio::time::timeout(Duration::from_secs(1), model.entered.notified())
            .await
            .expect("#1071 model entered deadline");
        if mode == "timeout-release" {
            tokio::time::advance(Duration::from_millis(2)).await;
        } else {
            cancel.cancel(None);
        }
        tokio::task::yield_now().await;
        let abort = model
            .abort
            .lock()
            .expect("#1071 abort observed")
            .clone()
            .expect("#1071 abort flag");
        negative_assert!(abort.is_aborted(), "#1071 title provider sees abort");
        negative_assert!(!task.is_finished(), "#1071 title waits after abort");
        if mode == "cancel-never" {
            task.abort();
            negative_assert!(
                task.await
                    .expect_err("#1071 never title cancellation")
                    .is_cancelled(),
                "#1071 never provider bounded by owner"
            );
            continue;
        }
        gate.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("#1071 title settle deadline")
            .expect("#1071 title task join")
            .expect("#1071 title provider result");
        let fixture = oracle();
        let expected = fixture["observations"]["titleWait"]
            .as_array()
            .expect("#1071 title wait rows")
            .iter()
            .find(|r| r["kind"] == mode)
            .expect("#1071 title wait row");
        negative_assert_eq!(
            json!(result.map(|s| String::from_utf16_lossy(s.units()))),
            expected["result"],
            "#1071 title wait oracle"
        );
    }
}
#[test]
#[ignore = "requires fixed read-only TS baseline and Node v22.22.0"]
fn regenerates_identically() {
    let result = std::panic::catch_unwind(|| {
        crate::test_baseline::regenerates_identically("native-process")
    });
    if let Err(payload) = result {
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if message.starts_with("#280 native-process/oracle.json differs from the regenerated ") {
            negative_assert!(false, "#1071 native oracle regenerated bytes");
        }
        // Baseline, process, parse and deadline failures are INVALID, not kills.
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn fixture_provenance() {
    crate::test_baseline::oracle(include_str!(
        "../../tests/fixtures/native-process/oracle.json"
    ));
}

const CONFIG: &str = "schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:0/v1\n      apiKey: mock-only\n      models: {mock: {}}\n";
#[tokio::test]
async fn production_profile_and_registration() {
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1071 profile options");
    let env = options.environment(Env::new());
    std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG).expect("#1071 profile config");
    assembly::validate_profile(&options, &env)
        .await
        .expect("#1071 empty integrations accepted");
    assembly::register_project(&options).expect("#1071 project registration");
    assembly::register_project(&options).expect("#1071 repeated project registration");
    let metadata = crate::metadata::Metadata {
        home: options.home.clone(),
        cwd: scratch.0.clone(),
        files: crate::metadata::NativeFiles,
    };
    negative_assert!(
        metadata
            .has_registered_project(&options.project)
            .expect("#1071 registered project path"),
        "#1071 project visible to metadata"
    );
    let selection = crate::session_model::execute(
        &metadata,
        &js(&json!({"projectKey":options.project,"sessionKey":"fresh"})),
        &env,
    )
    .expect("#1071 registered session model read");
    negative_assert_eq!(
        selection.get("effective").and_then(|v| v.get("model")),
        Some(&"mock".into()),
        "#1071 isolated model selection"
    );
    for (base, body) in [
        (
            &options.home,
            "{\"mcpServers\":{\"server\":{\"command\":\"must-not-run\"}}}",
        ),
        (&options.project.join(".pilotdeck"), "invalid json"),
    ] {
        std::fs::create_dir_all(base).expect("#1071 MCP parent");
        let file = base.join("mcp.json");
        std::fs::write(&file, body).expect("#1071 MCP configuration");
        negative_assert!(
            assembly::validate_profile(&options, &env).await.is_err(),
            "#1071 unsupported MCP refused"
        );
        std::fs::remove_file(file).expect("#1071 remove MCP fixture");
    }
    let skill = options.project.join(".pilotdeck/skills/fixture");
    std::fs::create_dir_all(&skill).expect("#1071 skill fixture");
    negative_assert!(
        assembly::validate_profile(&options, &env).await.is_err(),
        "#1071 unsupported contributions refused"
    );
    std::fs::remove_dir_all(options.project.join(".pilotdeck/skills"))
        .expect("#1071 remove skill fixture");
    std::fs::write(
        options.home.join("pilotdeck.yaml"),
        CONFIG.replace(
            "agent: {model: fixture/mock}",
            "agent: {model: fixture/mock, subagents: {}}",
        ),
    )
    .expect("#1071 subagent fixture");
    negative_assert!(
        assembly::validate_profile(&options, &env).await.is_err(),
        "#1071 unsupported subagents refused"
    );
    std::fs::write(
        options.home.join("pilotdeck.yaml"),
        format!("{CONFIG}modules:\n  skills: {{enabled: false}}\n"),
    )
    .expect("#1071 module fixture");
    use crate::runtime::project::ConfigSource;
    crate::runtime::project::NativeConfigSource::new(
        options.project.clone(),
        env.clone(),
        Some(options.home.clone()),
        None,
    )
    .load(&options.project)
    .expect("#1071 module fixture parses successfully");
    negative_assert!(
        assembly::validate_profile(&options, &env).await.is_err(),
        "#1071 unsupported module bindings refused"
    );
}

/// #2354: the production assembly's `/api/web` mount, held up end to end. `run`
/// composes the listener's plain-HTTP faces through [`super::production_api_web`]
/// and [`super::production_listener`]; this drives that exact composition —
/// the native metadata catalog over the WS lane's home, the WS lane's own
/// token — and gets the project list back through the real accept loop.
/// Deleting the `.with_api_web` mount in `production_listener` answers the
/// static 404 here instead of the project list, so the wiring cannot silently
/// disappear from the listener every other test composes for itself.
#[tokio::test]
async fn production_listener_mounts_the_api_web_group() {
    use crate::runtime::{
        connection::{Call, RequestError},
        turn::gateway::{self, GatewayRequests},
    };
    use pilotdeck_protocol::JsString;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        sync::oneshot,
        time::timeout,
    };
    struct NoRequests;
    impl GatewayRequests for NoRequests {
        fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
            Box::pin(async { Ok(JsValue::Null) })
        }
        fn request(&self, _: JsString, _: JsValue) -> Call<'_, Result<JsValue, RequestError>> {
            Box::pin(async { Ok(JsValue::Null) })
        }
    }
    let scratch = Scratch::new();
    let server = super::production_listener(
        "127.0.0.1:0"
            .parse()
            .expect("#2354 production listen address"),
        gateway::tests::native_ws_fixture(Arc::new(NoRequests)),
        "fixture-token".into(),
        Arc::new(|| None),
        super::production_api_web("fixture-token".into(), scratch.0.join("home")),
    )
    .await
    .expect("#2354 bind the production listener");
    let address = server
        .local_addr()
        .expect("#2354 production listener address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        std::time::Duration::from_secs(10),
    ));
    let mut stream = timeout(
        std::time::Duration::from_secs(10),
        TcpStream::connect(address),
    )
    .await
    .expect("#2354 connect deadline")
    .expect("#2354 connect to the production listener");
    stream
        .write_all(
            b"GET /api/web/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n",
        )
        .await
        .expect("#2354 write the project list request");
    let mut response = Vec::new();
    let mut buffer = [0u8; 4096];
    // The answer is a single chunked frame on a kept-alive connection, so the
    // terminal chunk ends the capture (no EOF to wait for).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !response.ends_with(b"0\r\n\r\n") {
        assert!(
            std::time::Instant::now() < deadline,
            "#2354 the production listener did not answer the project list"
        );
        let read = timeout(
            std::time::Duration::from_millis(500),
            stream.read(&mut buffer),
        )
        .await
        .expect("#2354 response read deadline");
        let read = read.expect("#2354 the production listener connection stays up");
        assert!(
            read > 0,
            "#2354 the production listener closed before answering"
        );
        response.extend_from_slice(&buffer[..read]);
    }
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "#2354 the production listener serves the api-web group with the WS lane's token: {text:?}"
    );
    assert!(
        text.contains("{\"projects\":[]}"),
        "#2354 the native catalog answers the project list over the production mount: {text:?}"
    );
    stop.send(()).expect("#2354 stop the production listener");
    timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("#2354 listener stop deadline")
        .expect("#2354 listener task join")
        .expect("#2354 the production listener drains cleanly");
}

// The ledger's route ids for the group, in `gateway.http:<METHOD> <path>`
// form; the five in-group probes below are built from these so the test
// cannot drift from the ledger it claims to hold up (`:projectKey` is the
// ledger's placeholder, `route_path` substitutes the encoded scratch
// root). The 404/boundary probes — `/api/web/nope`, the unknown files
// sub-path, the no-slash `/api/web` edge, the static lane — deliberately
// bypass `route_path`: they assert the ledger-external behavior *around*
// the group, not any registered route, so they carry no drift promise.
const LIST: &str = "GET /api/web/projects";
const DESCRIBE: &str = "GET /api/web/projects/:projectKey";
const TREE: &str = "GET /api/web/projects/:projectKey/files/tree";
const READ: &str = "GET /api/web/projects/:projectKey/files/read";
const WRITE: &str = "POST /api/web/projects/:projectKey/files/write";
const ROUTES: [&str; 5] = [LIST, DESCRIBE, TREE, READ, WRITE];
// The ledger's `gateway.http:` ids that name fallback lanes, not routes:
// they describe exactly the behavior the boundary probes assert (the
// plain 404 lane and the static fallback), so they carry no
// `<METHOD> <path>` shape on purpose — `enumerate.mjs` registers the
// same two.
const NON_ROUTE_LANES: [&str; 2] = ["<unmatched-404>", "static-fallback"];

// The shape decision the ledger cross-check below makes per `gateway.http:`
// id, extracted so a plain #[test] can pin every arm. fix-r3: `GET ` — a
// method, the separator, and no path at all — once slipped through the
// guard (`"".starts_with(' ')` is false) and the id then silently vanished
// from `held_up` instead of dying, the opposite of the cross-check's
// "dies here" promise.
#[derive(Debug, PartialEq)]
enum HttpLedgerId<'a> {
    Route(&'a str),
    NonRouteLane,
    Malformed,
}

fn http_ledger_id_shape(route: &str) -> HttpLedgerId<'_> {
    match route.split_once(' ') {
        // A real route: a non-empty method, one separator, and a path that
        // is neither empty (`GET `) nor a second separator's leading space
        // (`GET  /x`) — both die in the Malformed arm.
        Some((method, path))
            if !method.is_empty() && !path.is_empty() && !path.starts_with(' ') =>
        {
            HttpLedgerId::Route(path)
        }
        _ if NON_ROUTE_LANES.contains(&route) => HttpLedgerId::NonRouteLane,
        _ => HttpLedgerId::Malformed,
    }
}

// fix-r3 pin: every malformed route-id shape — a tab separator, a double
// space, a bare method, an empty path (`GET `), an empty method — lands in
// the dying arm, never a silent skip out of the cross-check's `held_up`.
#[test]
fn malformed_http_ledger_route_ids_die_including_the_empty_path() {
    for malformed in [
        "GET\t/api/web/projects",
        "GET  /api/web/projects",
        "GET",
        "GET ",
        " /api/web/projects",
    ] {
        negative_assert_eq!(
            http_ledger_id_shape(malformed),
            HttpLedgerId::Malformed,
            "#2395 malformed ledger route id must die, not vanish: {malformed:?}"
        );
    }
    for lane in NON_ROUTE_LANES {
        negative_assert_eq!(
            http_ledger_id_shape(lane),
            HttpLedgerId::NonRouteLane,
            "#2395 the fallback lane keeps its shapeless skip: {lane}"
        );
    }
    for route in ROUTES {
        let (_, path) = route.split_once(' ').expect("#2395 route id shape");
        negative_assert_eq!(
            http_ledger_id_shape(route),
            HttpLedgerId::Route(path),
            "#2395 the probed route keeps its path: {route}"
        );
    }
}

/// #2395: the `/api/web` group's presentation on the production listener,
/// held up per ledger entry (specs/product-coverage/entries.json, group
/// A678 — five routes `partial-native`, evidence the `serve_with_body`
/// mount): each of the five routes through the real accept loop with the
/// production catalog over a registered project, the group's own JSON 404s
/// (`/api/web/nope`, an unknown files sub-path) against the plain-text 404
/// the lanes outside the group answer (`/api/web` without the trailing
/// slash, and `/` for the static lane the production listener does not
/// serve — `with_static_root` stays test-only), plus the 401 before any
/// routing. The ledger half is machine-checked, not asserted in prose: the
/// test reads entries.json and dies unless its `/api/web` routes are
/// exactly the five probed (each still in group A678 at `partial-native`),
/// and the probe URLs are built from the same route ids, so neither side
/// can drift alone.
#[tokio::test]
async fn production_listener_presents_the_api_web_ledger_surface() {
    use crate::runtime::{
        connection::{Call, RequestError},
        turn::gateway::{self, GatewayRequests},
    };
    use pilotdeck_protocol::JsString;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        sync::oneshot,
        time::timeout,
    };
    struct NoRequests;
    impl GatewayRequests for NoRequests {
        fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>> {
            Box::pin(async { Ok(JsValue::Null) })
        }
        fn request(&self, _: JsString, _: JsValue) -> Call<'_, Result<JsValue, RequestError>> {
            Box::pin(async { Ok(JsValue::Null) })
        }
    }
    fn route_path(route: &str, encoded_root: &str) -> String {
        let (_, path) = route.split_once(' ').expect("#2395 route id shape");
        path.replace(":projectKey", encoded_root)
    }
    // The cross-check itself: the `gateway.http:` routes under `/api/web/`
    // in specs/product-coverage/entries.json must be exactly these five,
    // each still in group A678 at `partial-native`. A sixth route, a moved
    // group or a status flip in the ledger dies here instead of silently
    // outliving the probes.
    let ledger: Value = serde_json::from_str(include_str!(
        "../../../../specs/product-coverage/entries.json"
    ))
    .expect("#2395 ledger JSON");
    let mut held_up = Vec::new();
    for entry in ledger["entries"].as_array().expect("#2395 ledger entries") {
        let id = entry["id"].as_str().expect("#2395 entry id").to_string();
        let group = entry["group"]
            .as_str()
            .expect("#2395 entry group")
            .to_string();
        let status = entry["status"]
            .as_str()
            .expect("#2395 entry status")
            .to_string();
        let route = match id.strip_prefix("gateway.http:") {
            Some(route) => route,
            None => continue,
        };
        let path = match http_ledger_id_shape(route) {
            HttpLedgerId::Route(path) => path,
            HttpLedgerId::NonRouteLane => continue,
            // Any other shape (a tab separator, a double space, a missing
            // or empty path) must die here: `continue` would let the id
            // vanish from `held_up` silently — the opposite of the "dies
            // here" this cross-check promises.
            HttpLedgerId::Malformed => panic!("#2395 malformed ledger route id: {id}"),
        };
        if path.starts_with("/api/web/") {
            held_up.push((id, group, status));
        }
    }
    held_up.sort();
    let mut probed: Vec<(String, String, String)> = ROUTES
        .iter()
        .map(|route| {
            (
                format!("gateway.http:{route}"),
                "A678".to_string(),
                "partial-native".to_string(),
            )
        })
        .collect();
    probed.sort();
    negative_assert_eq!(
        held_up,
        probed,
        "#2395 the ledger's /api/web routes are exactly the five this test probes"
    );
    let scratch = Scratch::new();
    // One registered project in the WS lane's home — the same marker shape
    // `assembly::register_project` writes — with one file the tree/read/write
    // probes address.
    let project = scratch.0.join("project");
    std::fs::write(project.join("hello.txt"), "#2395 probe file\n")
        .expect("#2395 seed the project file");
    let registry = scratch.0.join("home").join("projects").join("pdrs-2395");
    std::fs::create_dir_all(&registry).expect("#2395 project registry directory");
    std::fs::write(registry.join(".cwd"), project.to_string_lossy().as_bytes())
        .expect("#2395 project marker");
    let encoded_root = project.to_string_lossy().replace('/', "%2F");
    let server = super::production_listener(
        "127.0.0.1:0"
            .parse()
            .expect("#2395 production listen address"),
        gateway::tests::native_ws_fixture(Arc::new(NoRequests)),
        "fixture-token".into(),
        Arc::new(|| None),
        super::production_api_web("fixture-token".into(), scratch.0.join("home")),
    )
    .await
    .expect("#2395 bind the production listener");
    let address = server
        .local_addr()
        .expect("#2395 production listener address");
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.run(
        async {
            let _ = stopped.await;
        },
        std::time::Duration::from_secs(10),
    ));

    // One request per connection; every lane here answers a chunked body, so
    // the terminal chunk ends the capture (no EOF to wait for).
    async fn exchange(address: std::net::SocketAddr, request: &str) -> String {
        let mut stream = timeout(
            std::time::Duration::from_secs(10),
            TcpStream::connect(address),
        )
        .await
        .expect("#2395 connect deadline")
        .expect("#2395 connect to the production listener");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("#2395 write the probe request");
        let mut response = Vec::new();
        let mut buffer = [0u8; 8192];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !response.ends_with(b"0\r\n\r\n") {
            assert!(
                std::time::Instant::now() < deadline,
                "#2395 the production listener did not answer the probe"
            );
            let read = timeout(
                std::time::Duration::from_millis(500),
                stream.read(&mut buffer),
            )
            .await
            .expect("#2395 response read deadline")
            .expect("#2395 the production listener connection stays up");
            assert!(
                read > 0,
                "#2395 the production listener closed before answering"
            );
            response.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8_lossy(&response).into_owned()
    }
    let get = |path: String| {
        format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\n\r\n"
        )
    };

    // The bearer check runs before any routing (#1348): an unauthorized GET
    // is the 401, not a 404.
    let unauthorized = exchange(
        address,
        &format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            route_path(LIST, &encoded_root)
        ),
    )
    .await;
    assert!(
        unauthorized.starts_with("HTTP/1.1 401"),
        "#2395 the group answers 401 before routing: {unauthorized:?}"
    );
    assert!(
        unauthorized.contains("\"code\":\"unauthorized\""),
        "#2395 the 401 body is the group's unauthorized JSON: {unauthorized:?}"
    );

    // Entry `GET /api/web/projects`: the native catalog lists the registered
    // project through the production composition.
    let listed = exchange(address, &get(route_path(LIST, &encoded_root))).await;
    assert!(
        listed.starts_with("HTTP/1.1 200"),
        "#2395 the project list route serves the native catalog: {listed:?}"
    );
    assert!(
        listed.contains("\"projects\":[") && listed.contains("\"projectKey\""),
        "#2395 the project list reports the registered project: {listed:?}"
    );

    // Entry `GET /api/web/projects/:projectKey`.
    let described = exchange(address, &get(route_path(DESCRIBE, &encoded_root))).await;
    assert!(
        described.starts_with("HTTP/1.1 200"),
        "#2395 the describe route serves the native catalog: {described:?}"
    );
    assert!(
        described.contains("\"fullPath\""),
        "#2395 the describe route answers the project summary: {described:?}"
    );

    // Entry `GET /api/web/projects/:projectKey/files/tree`.
    let tree = exchange(address, &get(route_path(TREE, &encoded_root))).await;
    assert!(
        tree.starts_with("HTTP/1.1 200") && tree.contains("hello.txt"),
        "#2395 the tree route lists the project file: {tree:?}"
    );

    // Entry `GET /api/web/projects/:projectKey/files/read`.
    let read = exchange(
        address,
        &get(format!(
            "{}?path=hello.txt",
            route_path(READ, &encoded_root)
        )),
    )
    .await;
    assert!(
        read.starts_with("HTTP/1.1 200") && read.contains("2395 probe file"),
        "#2395 the read route answers the file content: {read:?}"
    );

    // Entry `POST /api/web/projects/:projectKey/files/write`, read back on a
    // second request like the golden's round-trip cells.
    let body = r#"{"path":"written-2395.txt","content":"written by #2395"}"#;
    let written = exchange(
        address,
        &format!(
            "POST {} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer fixture-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            route_path(WRITE, &encoded_root),
            body.len()
        ),
    )
    .await;
    assert!(
        written.starts_with("HTTP/1.1 200") && written.contains("{\"ok\":true}"),
        "#2395 the write route commits and answers ok:true: {written:?}"
    );
    let reread = exchange(
        address,
        &get(format!(
            "{}?path=written-2395.txt",
            route_path(READ, &encoded_root)
        )),
    )
    .await;
    assert!(
        reread.starts_with("HTTP/1.1 200") && reread.contains("written by #2395"),
        "#2395 the written file reads back: {reread:?}"
    );

    // The group's own 404 category: an unknown path inside `/api/web/`.
    let no_route = exchange(address, &get("/api/web/nope".to_string())).await;
    assert!(
        no_route.starts_with("HTTP/1.1 404") && no_route.contains("\"code\":\"not_found\""),
        "#2395 an unknown /api/web path answers the group JSON 404: {no_route:?}"
    );
    let no_sub = exchange(
        address,
        &get(format!("/api/web/projects/{encoded_root}/files/nope")),
    )
    .await;
    assert!(
        no_sub.starts_with("HTTP/1.1 404") && no_sub.contains("\"code\":\"not_found\""),
        "#2395 an unknown files sub-path answers the group JSON 404: {no_sub:?}"
    );

    // The lane boundary: `/api/web` without the trailing slash never enters
    // the group, so the native plain-text 404 answers, not the group JSON.
    let boundary = exchange(address, &get("/api/web".to_string())).await;
    assert!(
        boundary.starts_with("HTTP/1.1 404") && boundary.contains("text/plain"),
        "#2395 the no-slash boundary answers the native plain 404: {boundary:?}"
    );
    assert!(
        !boundary.contains("application/json"),
        "#2395 the no-slash boundary does not answer the group JSON: {boundary:?}"
    );

    // The static lane the production listener does not serve (`static_root`
    // stays `None`): a UI-style path gets the native plain 404, never a file.
    let static_lane = exchange(address, &get("/".to_string())).await;
    assert!(
        static_lane.starts_with("HTTP/1.1 404"),
        "#2395 a static path answers 404 on the production listener: {static_lane:?}"
    );
    assert!(
        !static_lane.contains("<"),
        "#2395 the production listener serves no static or web-UI document: {static_lane:?}"
    );

    stop.send(()).expect("#2395 stop the production listener");
    timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("#2395 listener stop deadline")
        .expect("#2395 listener task join")
        .expect("#2395 the production listener drains cleanly");
}
#[test]
fn tool_policy_and_turn_environment() {
    let lsp = Arc::new(families::DefaultLsp::default());
    let all = tools::native_tools(None, None, lsp.clone(), &tools::SessionMcpTools::empty())
        .list()
        .expect("#1071 native tool catalog");
    negative_assert!(
        all.iter()
            .any(|tool| tool.fields().get("name") == Some(&"read_file".into())),
        "#1071 native read tool installed"
    );
    negative_assert!(
        tools::native_tools(
            Some(vec![]),
            None,
            lsp.clone(),
            &tools::SessionMcpTools::empty()
        )
        .list()
        .expect("#1071 restricted catalog")
        .is_empty(),
        "#1071 empty tool allowlist enforced"
    );
    let denied = tools::native_tools(
        Some(vec!["read_file".into()]),
        Some(vec!["read_file".into()]),
        lsp,
        &tools::SessionMcpTools::empty(),
    )
    .list()
    .expect("#1071 denied catalog");
    negative_assert!(denied.is_empty(), "#1071 tool denylist enforced");
    let environment = NativeTurnEnvironment(assembly::value([("CAPTURED", "present".into())]));
    let env = environment
        .build(None, &"/project".into(), &"session".into(), &"turn".into())
        .expect("#1071 turn environment");
    negative_assert_eq!(
        env.get("CAPTURED"),
        Some(&"present".into()),
        "#1071 captured turn environment"
    );
    negative_assert_eq!(
        env.get("PILOTDECK_SESSION_ID"),
        Some(&"session".into()),
        "#1071 turn session binding"
    );
    negative_assert!(
        environment
            .build(
                Some(&JsValue::Bool(true)),
                &"/project".into(),
                &"session".into(),
                &"turn".into()
            )
            .is_err(),
        "#1071 invalid turn base refused"
    );
}

// #1354: the main entry's default routing switch as a formal config item.
// Unset keeps the production default — proxy mode, #1303 option A — `proxy`
// states it explicitly, `native` is the only other routing, and every other
// value — the empty string and non-UTF-8 bytes included — fails closed, the
// #1369 shape.
#[test]
fn gateway_mode_config_matrix() {
    negative_assert!(
        matches!(gateway_mode(None), Ok(false)),
        "#1354 unset gateway mode keeps the proxy default (#1303 A)"
    );
    negative_assert_eq!(
        gateway_mode(Some(std::ffi::OsStr::new("proxy"))).ok(),
        Some(false),
        "#1354 explicit proxy mode"
    );
    negative_assert_eq!(
        gateway_mode(Some(std::ffi::OsStr::new("native"))).ok(),
        Some(true),
        "#1354 explicit native mode"
    );
    for value in [
        "", " ", "PROXY", "Native", "native ", " native", "true", "1", "proxy\n",
    ] {
        negative_assert!(
            gateway_mode(Some(std::ffi::OsStr::new(value))).is_err(),
            "#1354 invalid gateway mode {value:?} rejected"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        negative_assert!(
            gateway_mode(Some(std::ffi::OsStr::from_bytes(b"native\xff"))).is_err(),
            "#1354 non-UTF-8 gateway mode rejected"
        );
    }
}

// #1369: the controlled registry switch. Unset keeps the production default
// (registry on, no explicit state), on/off state the switch explicitly, and
// every other value — including the empty string — fails closed.
#[test]
fn run_registry_switch_matrix() {
    negative_assert!(
        matches!(assembly::run_registry_switch(&Env::new()), Ok(None)),
        "#1369 unset registry switch keeps the production default"
    );
    for (value, on) in [("on", true), ("off", false)] {
        let mut env = Env::new();
        env.insert("PILOTDECK_NATIVE_RUN_REGISTRY".into(), value.into());
        let switch = assembly::run_registry_switch(&env)
            .unwrap_or_else(|error| panic!("#1369 registry switch {value:?} parses: {error}"));
        let stated = match switch {
            Some(assembly::RunRegistrySwitch::On) => "on",
            Some(assembly::RunRegistrySwitch::Off) => "off",
            None => panic!("#1369 registry switch {value:?} states its value"),
        };
        negative_assert_eq!(stated, value, "#1369 explicit registry switch state");
        negative_assert_eq!(stated == "on", on, "#1369 explicit registry switch target");
    }
    for value in ["", "true", "ON", "1", " "] {
        let mut env = Env::new();
        env.insert("PILOTDECK_NATIVE_RUN_REGISTRY".into(), value.into());
        negative_assert!(
            assembly::run_registry_switch(&env).is_err(),
            "#1369 invalid registry switch value rejected"
        );
    }
}

// #1370: the controlled storage stimulus entry. Unset keeps the production
// default; a set value must name an existing directory and is returned as the
// control directory; anything else — including the empty string and a missing
// directory — fails closed at startup.
#[test]
fn storage_stimulus_switch_matrix() {
    negative_assert!(
        matches!(assembly::storage_stimulus_switch(&Env::new()), Ok(None)),
        "#1370 unset storage stimulus keeps the production default"
    );
    let scratch = Scratch::new();
    let control = scratch.0.join("control");
    std::fs::create_dir(&control).expect("#1370 control directory fixture");
    let mut env = Env::new();
    env.insert(
        "PILOTDECK_NATIVE_STORAGE_STIMULUS".into(),
        control.to_str().expect("#1370 control UTF8").into(),
    );
    let resolved =
        assembly::storage_stimulus_switch(&env).expect("#1370 existing control directory resolves");
    negative_assert_eq!(
        resolved.as_deref(),
        Some(control.as_path()),
        "#1370 stimulus switch returns the control directory"
    );
    for value in ["", " "] {
        let mut env = Env::new();
        env.insert("PILOTDECK_NATIVE_STORAGE_STIMULUS".into(), value.into());
        negative_assert!(
            assembly::storage_stimulus_switch(&env).is_err(),
            "#1370 empty stimulus switch value rejected"
        );
    }
    let missing = scratch.0.join("missing-control");
    let mut env = Env::new();
    env.insert(
        "PILOTDECK_NATIVE_STORAGE_STIMULUS".into(),
        missing.to_str().expect("#1370 missing UTF8").into(),
    );
    let error = assembly::storage_stimulus_switch(&env)
        .expect_err("#1370 missing stimulus directory rejected");
    negative_assert!(
        error.to_string().contains("existing directory"),
        "#1370 missing stimulus directory reports the explicit gate: {error}"
    );
}

// #1371: the controlled loop stimulus entry, the third startup switch in this
// family. Unset keeps the production default; a set value must name an
// existing directory and is returned as the control directory; anything else
// — including the empty string and a missing directory — fails closed at
// startup. Note the value is only a directory here: whether it carries a plan
// is decided later, and an armed directory without one stays inert.
#[test]
fn loop_stimulus_switch_matrix() {
    negative_assert!(
        matches!(assembly::loop_stimulus_switch(&Env::new()), Ok(None)),
        "#1371 unset loop stimulus keeps the production default"
    );
    let scratch = Scratch::new();
    let control = scratch.0.join("control");
    std::fs::create_dir(&control).expect("#1371 control directory fixture");
    let mut env = Env::new();
    env.insert(
        "PILOTDECK_NATIVE_LOOP_STIMULUS".into(),
        control.to_str().expect("#1371 control UTF8").into(),
    );
    let resolved =
        assembly::loop_stimulus_switch(&env).expect("#1371 existing control directory resolves");
    negative_assert_eq!(
        resolved.as_deref(),
        Some(control.as_path()),
        "#1371 loop switch returns the control directory"
    );
    for value in ["", " "] {
        let mut env = Env::new();
        env.insert("PILOTDECK_NATIVE_LOOP_STIMULUS".into(), value.into());
        negative_assert!(
            assembly::loop_stimulus_switch(&env).is_err(),
            "#1371 empty loop switch value rejected"
        );
    }
    let missing = scratch.0.join("missing-control");
    let mut env = Env::new();
    env.insert(
        "PILOTDECK_NATIVE_LOOP_STIMULUS".into(),
        missing.to_str().expect("#1371 missing UTF8").into(),
    );
    let error = assembly::loop_stimulus_switch(&env)
        .expect_err("#1371 missing stimulus directory rejected");
    negative_assert!(
        error.to_string().contains("existing directory"),
        "#1371 missing stimulus directory reports the explicit gate: {error}"
    );
}

// A runs.json directory makes opening the persistent registry fail, so only
// the off switch can complete startup: unset and "on" pin the production
// default, and an invalid value is rejected before the file is touched.
#[tokio::test]
async fn run_registry_switch_controls_startup() {
    for (value, starts, gate) in [
        (Some("off"), true, None),
        (None, false, None),
        (Some("on"), false, None),
        (Some("invalid"), false, Some("must be exactly on or off")),
    ] {
        let scratch = Scratch::new();
        let options = Options::parse(scratch.args()).expect("#1369 registry switch options");
        std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG)
            .expect("#1369 registry switch config");
        std::fs::create_dir(options.home.join("runs.json")).expect("#1369 runs directory fixture");
        let mut inherited = Env::new();
        if let Some(value) = value {
            inherited.insert("PILOTDECK_NATIVE_RUN_REGISTRY".into(), value.into());
        }
        let outcome = assembly::start_observed(
            options.clone(),
            options.environment(inherited),
            Arc::new(|_, result| Box::pin(async move { result })),
            Arc::new(|error| eprintln!("native project cleanup: {error}")),
        )
        .await;
        if !starts {
            let error = match outcome {
                Ok(_) => panic!("#1369 registry switch {value:?} startup refused"),
                Err(error) => error,
            };
            if let Some(gate) = gate {
                negative_assert!(
                    error.to_string().contains(gate),
                    "#1369 invalid registry switch reports the explicit gate: {error}"
                );
            }
            continue;
        }
        let host = crate::test_negative_receipt::expect_ok(
            outcome,
            "#1369 off registry switch starts without the run file",
        );
        negative_assert!(
            options.home.join("runs.json").is_dir(),
            "#1369 off registry switch leaves the run path untouched"
        );
        host.request_shutdown(options.drain_budget);
        tokio::time::timeout(std::time::Duration::from_secs(10), host.stopped())
            .await
            .expect("#1369 off registry switch host stop deadline")
            .expect("#1369 off registry switch host stopped clean");
    }
}

// TS ui/server/routes/uploads.js:73-76: the assembly, not a request, drives
// `cleanup_expired`. `setInterval` first fires after one interval (not at
// t=0) and keeps firing; each round sweeps both TTL families (upload records
// and crash-orphan leases) (#1439).
#[tokio::test]
async fn upload_cleanup_runs_on_the_assembly_schedule() {
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1439 upload cleanup options");
    std::fs::write(options.home.join("pilotdeck.yaml"), CONFIG)
        .expect("#1439 upload cleanup config");
    let project_key = std::fs::canonicalize(&options.project).expect("#1439 project canonical");
    let uploads = options.project.join(".tmp/chat-uploads");
    let leases = options.project.join(".tmp/chat-upload-leases");
    let seed = |id: &str, expires_at: &str| {
        let dir = uploads.join(id);
        std::fs::create_dir_all(&dir).expect("#1439 upload cleanup seed directory");
        let record = json!({
            "uploadId": id,
            "projectKey": project_key.to_string_lossy(),
            "status": "created",
            "expiresAt": expires_at,
        });
        std::fs::write(dir.join("metadata.json"), format!("{record}\n"))
            .expect("#1439 upload cleanup seed metadata");
        let lease = leases.join(id).join("lease-1");
        std::fs::create_dir_all(&lease).expect("#1439 lease cleanup seed directory");
        std::fs::write(
            lease.join("lease.json"),
            format!("{}\n", json!({ "expiresAt": expires_at })),
        )
        .expect("#1439 lease cleanup seed lease");
    };
    // Seeded before the assembly starts, so a sweep at t=0 would take them.
    seed("upload-expired", "1970-01-01T00:00:00.000Z");
    seed("upload-live", "2100-01-01T00:00:00.000Z");
    let expired_upload = uploads.join("upload-expired");
    let expired_lease = leases.join("upload-expired").join("lease-1");
    // The t≈100 ms probe below only means "the first sweep waits one interval"
    // while the clamp leaves this period alone; a raised floor would push the
    // first sweep past the probe and make it vacuous.
    let cleanup_period = std::time::Duration::from_secs(2);
    assert!(
        cleanup_period >= crate::upload::MIN_CLEANUP_INTERVAL,
        "this test relies on spawn_cleanup honouring its period verbatim"
    );
    let host = crate::test_negative_receipt::expect_ok(
        assembly::start_observed_with_upload_cleanup(
            options.clone(),
            options.environment(Env::new()),
            cleanup_period,
            Arc::new(|_, result| Box::pin(async move { result })),
            Arc::new(|error| eprintln!("native project cleanup: {error}")),
        )
        .await,
        "#1439 upload cleanup assembly starts",
    );
    // t ≈ 100 ms « 2 s: the first sweep lands after one interval, not at t=0.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    negative_assert!(
        expired_upload.exists(),
        "#1439 the first sweep waits one interval (uploads)"
    );
    negative_assert!(
        expired_lease.exists(),
        "#1439 the first sweep waits one interval (leases)"
    );
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while expired_upload.exists() || expired_lease.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("#1439 the first sweep removes both expired TTL families");
    negative_assert!(
        uploads.join("upload-live").exists(),
        "#1439 upload cleanup keeps the unexpired record"
    );
    negative_assert!(
        leases.join("upload-live").join("lease-1").exists(),
        "#1439 lease cleanup keeps the unexpired lease"
    );
    // A record that appears after the first round is removed by a later one:
    // a one-shot sweep would leave it behind forever.
    seed("upload-round-2", "1970-01-01T00:00:00.000Z");
    let round_2_upload = uploads.join("upload-round-2");
    let round_2_lease = leases.join("upload-round-2").join("lease-1");
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while round_2_upload.exists() || round_2_lease.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("#1439 the schedule keeps sweeping after the first round");
    host.request_shutdown(options.drain_budget);
    tokio::time::timeout(std::time::Duration::from_secs(10), host.stopped())
        .await
        .expect("#1439 upload cleanup host stop deadline")
        .expect("#1439 upload cleanup host stopped clean");
}

// TS InProcessGateway.ts:2093-2095 and :2525-2544: without a configured
// registry, describeServer drops the run capabilities and every run read
// fails with CAPABILITY_UNAVAILABLE; with one, they are listed and served.
struct NoSessions;
impl crate::runtime::project::ProjectRuntimeFactory for NoSessions {
    fn stage(
        &self,
        _: &std::path::Path,
        _: &mut crate::runtime::project::ResourceBundle,
    ) -> crate::runtime::project::RuntimeResult {
        panic!("#1369 registry switch requests must not stage resources")
    }
}
impl crate::runtime::session::SessionFactory for NoSessions {
    fn create(
        &self,
        _: crate::runtime::session::SessionContext,
        _: Option<crate::runtime::session::LiveSessionHandle>,
    ) -> crate::runtime::refresh::BoxFuture<
        Result<crate::runtime::session::LiveSessionHandle, crate::runtime::project::RuntimeError>,
    > {
        panic!("#1369 registry switch requests must not create sessions")
    }
}

#[tokio::test]
async fn requests_registry_switch_gates_run_reads() {
    use crate::runtime::turn::gateway::GatewayRequests;
    let scratch = Scratch::new();
    let options = Options::parse(scratch.args()).expect("#1369 requests options");
    let project = Arc::new(crate::runtime::project::ProjectRuntimeRegistry::new(
        options.project.clone(),
        Arc::new(NoSessions),
    ));
    let mut owner_options = crate::runtime::session::SessionOwnerOptions::new(Arc::new(NoSessions));
    owner_options.idle_sweep_interval_ms = 0;
    let owner = crate::runtime::session::SessionOwner::new(owner_options);
    let reference = js(&json!({"sessionKey":"session","runId":"run"}));
    let runs = Arc::new(
        crate::runtime::turn::publish::FileRegistry::new(
            crate::storage::FileRunRegistry::open(scratch.0.join("runs-on.json"))
                .expect("#1369 requests run file"),
        )
        .expect("#1369 requests run registry"),
    );
    for (runs, on) in [(Some(runs), true), (None, false)] {
        let requests = requests::Requests {
            options: options.clone(),
            env: options.environment(Env::new()),
            snapshot_enabled: false,
            memory: Arc::new(crate::runtime::memory::gateway::MemoryGateway::disabled()),
            coordinator: Arc::default(),
            runs,
            owner: owner.clone(),
            project: project.clone(),
            replacement: Arc::new(crate::runtime::replacement::ReplacementCoordinator::new(
                Default::default(),
                Arc::new(crate::runtime::session::SessionOwner::new({
                    let mut o =
                        crate::runtime::session::SessionOwnerOptions::new(Arc::new(NoSessions));
                    o.idle_sweep_interval_ms = 0;
                    o
                })),
                5000,
                None,
            )),
            recorders: Arc::default(),
            interactions: Arc::default(),
            mcp: requests::Requests::stub_mcp(&owner),
            async_hook_registry: None,
            // #1600: match production (connected), not the retired
            // observer-coupled bus.
            user_dialog_bus: Arc::new(std::sync::Mutex::new(
                crate::user_dialog::GatewayUserDialogBus::connected(
                    crate::user_dialog::DialogRuntime::default(),
                ),
            )),
            upload_gateway: Arc::new(crate::upload::gateway::UploadGateway::unavailable()),
            prepare_weixin_login: None,
            cron: None,
            always_on: None,
            settings: requests::Requests::stub_settings(&owner),
            session_overrides: Arc::default(),
        };
        let described = requests
            .describe_server()
            .await
            .expect("#1369 registry switch describe_server");
        let described = json_value(&described);
        let capabilities = described["capabilities"]
            .as_array()
            .expect("#1369 registry switch capabilities");
        // #3143 fix-r2 (DS sibling#4): the run-read names have one owner
        // (`methods/runs.rs`), the same single-source rule fix-r1 P2#6
        // pinned in management_tests.rs.
        for &capability in crate::native_process::methods::RUN_METHODS {
            negative_assert_eq!(
                capabilities
                    .iter()
                    .any(|listed| listed.as_str() == Some(capability)),
                on,
                "#1369 describe_server gates run capabilities"
            );
        }
        // #1642: the memory pair and the cron six are gated the same way as
        // the run reads above — `capabilities.rs` consults the bound memory
        // gateway and the configured cron runtime, not just the static
        // method registry. Both loop bodies keep memory disabled and cron
        // unconfigured, so neither family may appear in either iteration.
        for name in ["memory_list", "memory_wipe"] {
            negative_assert!(
                !capabilities
                    .iter()
                    .any(|listed| listed.as_str() == Some(name)),
                "#1642 describe_server gates memory capabilities on the bound gateway"
            );
        }
        for name in crate::runtime::cron::CRON_METHODS {
            negative_assert!(
                !capabilities
                    .iter()
                    .any(|listed| listed.as_str() == Some(name)),
                "#1642 describe_server gates cron capabilities on the configured runtime"
            );
        }
        if on {
            let result = crate::test_negative_receipt::expect_ok(
                requests.request("run_get".into(), reference.clone()).await,
                "#1369 run_get with a registry resolves",
            );
            negative_assert!(
                matches!(result, JsValue::Null),
                "#1369 run_get for an unregistered run is null"
            );
            let error = crate::test_negative_receipt::expect_err(
                requests
                    .request("run_events".into(), reference.clone())
                    .await,
                "#1369 run_events reads the configured registry",
            );
            negative_assert_eq!(
                error.code,
                "RUN_NOT_FOUND",
                "#1369 run_events with a registry reports the run"
            );
        } else {
            // #3143 fix-r2 (DS sibling#4): same single source as the loop
            // above -- the three run reads are `runs::METHODS` verbatim.
            for &method in crate::native_process::methods::RUN_METHODS {
                let error = crate::test_negative_receipt::expect_err(
                    requests.request(method.into(), reference.clone()).await,
                    "#1369 run read without a registry is unavailable",
                );
                negative_assert_eq!(
                    error.code,
                    "CAPABILITY_UNAVAILABLE",
                    "#1369 {method} without a registry is unavailable"
                );
                negative_assert!(
                    error
                        .message
                        .equals_str("Persistent run registry is unavailable."),
                    "#1369 {method} unavailable message matches TS"
                );
            }
        }
    }
}

#[path = "review_tests.rs"]
mod review;

#[path = "metadata_cwd_tests.rs"]
mod metadata_cwd;

#[test]
fn session_tool_results_dir_layout_and_sanitization() {
    let project = std::path::Path::new("/pdrs-897-project");
    // ProjectSessionStorage.ts:413: resolve(projectRoot, ".pilotdeck",
    // "tool-results", safeId) with safeId from sanitizeSessionIdForPath
    // (ProjectSessionStorage.ts:239-245: `/` and `\` runs collapse to `-`).
    negative_assert_eq!(
        super::tools::session_tool_results_dir(project, &JsString::from("session-897")),
        std::path::PathBuf::from("/pdrs-897-project/.pilotdeck/tool-results/session-897"),
        "#897 session tool-results directory keeps the project-relative layout"
    );
    negative_assert_eq!(
        super::tools::session_tool_results_dir(project, &JsString::from("team/always\\on")),
        std::path::PathBuf::from("/pdrs-897-project/.pilotdeck/tool-results/team-always-on"),
        "#897 session tool-results directory sanitizes the session id"
    );
}
