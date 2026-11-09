//! Process-level precheck failures; the owned mock test covers valid fallback.
use std::{
    fs,
    io::Read,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn output(command: &mut Command) -> Output {
    let mut child = OwnedChild(
        command
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
            "config precheck exceeded five seconds"
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
fn binding_precheck_errors_exit_one_and_host_required_stays_explicit() {
    let root = std::env::temp_dir().join(format!("pdrs-d1-config-exit-{}", std::process::id()));
    fs::create_dir_all(root.join("project")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let bundled = root.join("bundled");
    fs::create_dir_all(bundled.join("fixture")).unwrap();
    fs::write(
        bundled.join("fixture/SKILL.md"),
        "---\nname: fixture\ndescription: Offline fixture.\n---\nNo operation.\n",
    )
    .unwrap();
    let config = root.join("pilotdeck.yaml");
    for (binding, yaml, code, expected) in [
        (
            "true",
            "a: [\n",
            1,
            "Failed to parse default config YAML.\n",
        ),
        (
            "true",
            "schemaVersion: 1\n",
            1,
            "Config must contain an agent section.\n",
        ),
        (
            "false",
            "schemaVersion: 1\nagent: {model: constructor/m}\nmodel: {}\n",
            2,
            "pilotdeck-rs TODO: config requiring JavaScript host support is not implemented yet.\n",
        ),
    ] {
        fs::write(&config, yaml).unwrap();
        let output = output(
            Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
                .current_dir(root.join("project"))
                .env_clear()
                .env("HOME", root.join("home"))
                .env("PILOT_HOME", root.join("home"))
                .env("PILOTDECK_CONFIG_PATH", &config)
                .env("PILOTDECK_BUNDLED_SKILLS_DIR", &bundled)
                .env("STAFFDECK_PROJECT_BINDING_ENABLED", binding),
        );
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(String::from_utf8(output.stderr).unwrap(), expected);
    }
    fs::remove_dir_all(root).unwrap();
}
