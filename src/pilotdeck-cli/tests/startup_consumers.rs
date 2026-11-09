//! Fixed TS consumers: exact process bytes and product files, no network.
#![cfg(target_os = "macos")]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    os::unix::fs::DirBuilderExt,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};
// Reuse its owned-child kill/reap and 15s exit deadline; unrelated SDK mock
// entrypoints in this shared test module are intentionally unused here.
#[allow(dead_code)]
#[path = "common/sdk_exit.rs"]
mod sdk_exit;
use sdk_exit::{collect_bounded, spawn};

const ORACLE: &str = include_str!("fixtures/startup-home/consumers-oracle.json");
const ORACLE_SHA: &str = "1cab089847ed870e115f19b07ed411e05d828ab27b4d1416ef7a7cb2102ee40b";
const BOUND: Duration = Duration::from_secs(15);
const FILE_CAP: u64 = 1_048_576;
struct Scratch {
    root: PathBuf,
    retain: bool,
}
impl Scratch {
    fn new() -> Self {
        let requested = std::env::var_os("PDRS_STARTUP_CONSUMERS_EVIDENCE");
        let root = match &requested {
            Some(path) => {
                let path = Path::new(path);
                assert!(path.is_absolute(), "evidence path must be absolute");
                fs::canonicalize(path.parent().expect("evidence parent"))
                    .expect("existing evidence parent")
                    .join(path.file_name().expect("new evidence directory name"))
            }
            None => fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("pdrs-startup-consumers-{}", uuid::Uuid::new_v4())),
        };
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("exclusive evidence directory");
        fs::create_dir(root.join("harness")).unwrap();
        Self {
            root,
            retain: requested.is_some(),
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.retain {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

// Same deny-by-default Rust rules as scripts/cli-parity/policy.py, narrowed to
// this case and executable. No Node/dependency tree or user home is readable.
fn profile(runtime: &Path, executable: &Path) -> String {
    let dirs: Vec<PathBuf> = [
        runtime,
        Path::new("/System"),
        Path::new("/usr/lib"),
        Path::new("/Library/Apple"),
    ]
    .into_iter()
    .map(Path::to_path_buf)
    .collect();
    let files: Vec<PathBuf> = [
        executable,
        Path::new("/"),
        Path::new("/dev/null"),
        Path::new("/dev/urandom"),
        Path::new("/dev/random"),
    ]
    .into_iter()
    .map(Path::to_path_buf)
    .collect();
    let quote =
        |path: &Path| serde_json::to_string(path.to_str().expect("UTF8 fixture path")).unwrap();
    let readable = dirs
        .iter()
        .map(|p| format!("(subpath {})", quote(p)))
        .chain(files.iter().map(|p| format!("(literal {})", quote(p))))
        .collect::<Vec<_>>()
        .join(" ");
    let ancestors = dirs
        .iter()
        .chain(&files)
        .flat_map(|p| p.ancestors().skip(1))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|p| format!("(literal {})", quote(p)))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "(version 1)\n(deny default)\n(allow sysctl-read)\n(allow file-read-metadata {readable} {ancestors})\n(allow file-read-data file-map-executable {readable})\n(allow file-write* (subpath {}))\n(allow file-read* file-write* (literal \"/dev/null\") (literal \"/dev/stdin\") (literal \"/dev/stdout\") (literal \"/dev/stderr\"))\n(allow process-exec (literal {}))\n(deny network*)\n(deny process-fork)\n",
        quote(runtime),
        quote(executable)
    )
}
fn command(runtime: &Path, executable: &Path, policy: &Path) -> Command {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .args(["-f"])
        .arg(policy)
        .arg(executable)
        .env_clear()
        .current_dir(runtime)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("NO_COLOR", "1")
        .env("PATH", runtime.join("tmp/bin"))
        .env("ANALYTICS_ENABLED", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in [
        "HOME",
        "USERPROFILE",
        "CFFIXED_USER_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "TMPDIR",
        "TMP",
        "TEMP",
    ] {
        command.env(key, runtime.join("tmp"));
    }
    command
}
async fn output(command: &mut Command) -> Output {
    collect_bounded(spawn(command, &json!({})), BOUND).await
}
fn record(root: &Path, name: &str, output: &Output, files: &BTreeMap<String, Vec<u8>>) {
    fs::write(root.join("harness").join(format!("{name}.json")), serde_json::to_vec_pretty(&json!({
        "exit": output.status.code(), "stdoutBytes": &output.stdout, "stderrBytes": &output.stderr,
        "afterBytes": files,
    })).unwrap()).unwrap();
}
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<String, Vec<u8>>, count: &mut usize) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap();
            // Identical predeclared exclusion as the TS collector. Rust creates
            // no collector logs; every other product file is compared.
            if relative.components().next() == Some(Component::Normal("tmp".as_ref())) {
                continue;
            }
            *count += 1;
            assert!(*count <= 256, "unexpectedly large product tree");
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                visit(root, &path, files, count);
            } else {
                assert!(
                    metadata.is_file(),
                    "unexpected symlink/special output: {path:?}"
                );
                assert!(
                    metadata.len() <= FILE_CAP,
                    "unexpectedly large product file: {path:?}"
                );
                files.insert(relative.to_str().unwrap().into(), fs::read(&path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files, &mut 0);
    files
}

#[test]
#[ignore = "spawned under the case Seatbelt policy by the parent test"]
fn startup_sandbox_network_and_fork_child() {
    let address = std::env::var("PDRS_STARTUP_GUARD_ADDRESS").expect("owned listener address");
    let error = TcpStream::connect_timeout(&address.parse().unwrap(), Duration::from_secs(2))
        .expect_err("Seatbelt must reject the owned loopback connection");
    assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    println!("STARTUP_GUARD_CONNECT_EPERM");
    // Exec of this exact binary is allowed by the policy, so an exec-path
    // denial cannot masquerade as the fork-denial witness.
    let error = Command::new(std::env::current_exe().unwrap())
        .arg("--list")
        .status()
        .expect_err("Seatbelt must reject child creation");
    assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    println!("STARTUP_GUARD_FORK_EPERM");
}

#[tokio::test(flavor = "current_thread")]
async fn six_fixed_ts_consumers_match_exact_process_bytes_and_all_product_files() {
    assert!(
        Path::new("/usr/bin/sandbox-exec").is_file(),
        "unsupported: sandbox-exec required, no bare-run fallback"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(ORACLE.as_bytes())),
        ORACLE_SHA
    );
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(
        oracle["baseline"],
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    let scratch = Scratch::new();
    // Verify the actual kernel policy before executing any product case.
    let guard = scratch.root.join("guard");
    fs::create_dir(&guard).unwrap();
    fs::create_dir(guard.join("tmp")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let positive = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    let accept_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match listener.accept() {
            Ok(connection) => {
                drop(connection);
                break;
            }
            Err(error)
                if error.kind() == ErrorKind::WouldBlock
                    && tokio::time::Instant::now() < accept_deadline =>
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Err(error) => panic!("owned listener positive control: {error}"),
        }
    }
    drop(positive);
    fs::write(
        scratch.root.join("harness/network-positive.json"),
        serde_json::to_vec_pretty(&json!({
            "ownedAddress": address.to_string(), "outsideProfileConnectionAccepted": true,
        }))
        .unwrap(),
    )
    .unwrap();
    let test_binary = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let guard_profile = scratch.root.join("harness/guard.sb");
    fs::write(&guard_profile, profile(&guard, &test_binary)).unwrap();
    let result = output(
        command(&guard, &test_binary, &guard_profile)
            .env("PDRS_STARTUP_GUARD_ADDRESS", address.to_string())
            .args([
                "--ignored",
                "--exact",
                "startup_sandbox_network_and_fork_child",
                "--nocapture",
            ]),
    )
    .await;
    record(&scratch.root, "sandbox-guard", &result, &BTreeMap::new());
    assert!(result.status.success(), "Seatbelt guard failed: {result:?}");
    for marker in [
        b"STARTUP_GUARD_CONNECT_EPERM".as_slice(),
        b"STARTUP_GUARD_FORK_EPERM".as_slice(),
    ] {
        assert!(
            result
                .stdout
                .windows(marker.len())
                .any(|part| part == marker),
            "guard helper must actually execute: {result:?}"
        );
    }
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_pilotdeck")).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 6);
    let mut seen = BTreeSet::new();
    for case in cases {
        let id = case["id"].as_str().unwrap();
        assert!(
            [
                "legacy-empty",
                "legacy-relative",
                "migrate-empty",
                "migrate-relative",
                "chat-empty",
                "chat-relative"
            ]
            .contains(&id)
        );
        assert!(seen.insert(id));
        assert!(case["networkAttempts"].as_array().unwrap().is_empty());
        let runtime = scratch.root.join(id);
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let expand = |s: &str| s.replace("<CASE>", runtime.to_str().unwrap()).into_bytes();
        for (name, text) in case["inputs"].as_object().unwrap() {
            let relative = Path::new(name);
            assert!(
                relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            );
            let path = runtime.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, expand(text.as_str().unwrap())).unwrap();
        }
        fs::create_dir(runtime.join("tmp")).unwrap();
        let policy = scratch.root.join("harness").join(format!("{id}.sb"));
        fs::write(&policy, profile(&runtime, &executable)).unwrap();
        let mut child = command(&runtime, &executable, &policy);
        child
            .env("HOME", case["HOME"].as_str().unwrap())
            .env("PILOT_HOME", case["PILOT_HOME"].as_str().unwrap())
            .env("PILOTDECK_CONFIG_PATH", runtime.join("blocked.yaml"))
            .env("PILOTDECK_BUNDLED_SKILLS_DIR", runtime.join("bundled"));
        for argument in case["argv"].as_array().unwrap() {
            child.arg(argument.as_str().unwrap());
        }
        let result = output(&mut child).await;
        let after = snapshot(&runtime);
        record(&scratch.root, id, &result, &after);
        assert_eq!(
            result.status.code(),
            case["code"].as_i64().map(|n| n as i32),
            "{id} exit"
        );
        assert_eq!(
            result.stdout,
            expand(case["stdout"].as_str().unwrap()),
            "{id} stdout bytes"
        );
        assert_eq!(
            result.stderr,
            expand(case["stderr"].as_str().unwrap()),
            "{id} stderr bytes"
        );
        let expected: BTreeMap<_, _> = case["after"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, value)| (name.clone(), expand(value.as_str().unwrap())))
            .collect();
        assert_eq!(after, expected, "{id} all product file bytes");
    }
}
