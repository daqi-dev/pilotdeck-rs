//! Serve validates explicit authorities before creating a host or listener.
use std::{
    ffi::OsString,
    fs,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("pdrs-control-serve-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        for dir in ["home", "project"] {
            fs::create_dir(root.join(dir)).unwrap();
        }
        fs::write(root.join("token"), b"serve-test-token\n").unwrap();
        fs::write(root.join("sidecar"), b"unused parse-only fixture").unwrap();
        Self(root)
    }
    fn args(&self, listen: &str) -> Vec<OsString> {
        let mut args = vec![OsString::from("serve")];
        for (key, value) in [
            ("--home", self.0.join("home")),
            ("--project", self.0.join("project")),
            ("--token-file", self.0.join("token")),
            ("--sidecar", self.0.join("sidecar")),
        ] {
            args.push(key.into());
            args.push(value.into_os_string());
        }
        args.extend([OsString::from("--listen"), OsString::from(listen)]);
        args
    }
    fn unchanged(&self) {
        assert_eq!(
            fs::read(self.0.join("token")).unwrap(),
            b"serve-test-token\n"
        );
        assert_eq!(
            fs::read(self.0.join("sidecar")).unwrap(),
            b"unused parse-only fixture"
        );
        for dir in ["home", "project"] {
            assert_eq!(fs::read_dir(self.0.join(dir)).unwrap().count(), 0);
        }
        assert_eq!(fs::read_dir(&self.0).unwrap().count(), 4);
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn run(scratch: &Scratch, args: &[OsString]) -> Output {
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_pilotdeck-control"))
            .env_clear()
            .current_dir(&scratch.0)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "serve did not finish argument validation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    Output {
        status: child.0.wait().unwrap(),
        stdout,
        stderr,
    }
}
#[test]
fn serve_rejects_missing_duplicate_unknown_and_unsafe_arguments_without_state_or_bind() {
    let scratch = Scratch::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let valid = scratch.args(&listener.local_addr().unwrap().to_string());
    let mut duplicate = valid.clone();
    duplicate.extend(["--home".into(), scratch.0.join("home").into_os_string()]);
    let mut unknown = valid.clone();
    unknown.extend(["--url".into(), "ws://127.0.0.1:1/ws".into()]);
    let mut missing = valid.clone();
    missing.push("--drain-ms".into());
    let mut overlap = valid.clone();
    let index = overlap.iter().position(|arg| arg == "--project").unwrap();
    overlap[index + 1] = scratch.0.join("home").into_os_string();
    let mut relative = valid.clone();
    let index = relative.iter().position(|arg| arg == "--home").unwrap();
    relative[index + 1] = "home".into();
    for (args, expected) in [
        (vec!["serve".into()], "native mode requires --home"),
        (duplicate, "duplicate native option: --home"),
        (unknown, "unknown native option: --url"),
        (missing, "missing value for --drain-ms"),
        (
            overlap,
            "native home and project must be separate directories",
        ),
        (relative, "native home must be an absolute path"),
        (
            scratch.args("0.0.0.0:12345"),
            "native listen requires 127.0.0.1 and a high port (or 0)",
        ),
        (
            scratch.args("127.0.0.1:1"),
            "native listen requires 127.0.0.1 and a high port (or 0)",
        ),
    ] {
        let output = run(&scratch, &args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("{expected}\n")
        );
        scratch.unchanged();
        assert!(
            matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock)
        );
    }
}
#[test]
fn serve_help_is_explicit_and_serve_cannot_be_a_call_mode_positional() {
    let scratch = Scratch::new();
    let output = run(&scratch, &["serve".into(), "--help".into()]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.starts_with("pilotdeck-control serve --home"));
    assert!(help.contains("stdout has no JSON readiness protocol"));
    let output = run(
        &scratch,
        &[
            "--url".into(),
            "ws://127.0.0.1:1/ws".into(),
            "--token-file".into(),
            scratch.0.join("token").into_os_string(),
            "serve".into(),
        ],
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["kind"], "input_error");
    scratch.unchanged();
}
