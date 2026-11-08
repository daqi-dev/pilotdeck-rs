//! Opt-in configuration IO limits for the new CLI entry points. The established
//! Gateway loaders and NativeFiles retain their original behavior.
use super::{Config, Env, Error};
use pilotdeck_protocol::{JsString, JsValue};
use std::path::Path;

#[cfg(unix)]
const FILE_LIMIT: usize = 8 * 1024 * 1024;
#[cfg(unix)]
const TOTAL_LIMIT: usize = 32 * 1024 * 1024;

/// Fresh authorization lookup through the same bounded configuration Files
/// facade. No project stage, SOP bundle, or write is performed.
pub(crate) fn project_binding_target_bounded(
    home: &Path,
    env: &Env,
    project: &str,
) -> Result<Option<JsString>, Error> {
    #[cfg(unix)]
    {
        unix::binding_target(home, env, project)
    }
    #[cfg(not(unix))]
    {
        let _ = (home, env, project);
        Err(Error::new(
            "CONFIG_BOUNDED_UNSUPPORTED",
            "Bounded configuration loading requires Unix file descriptors.",
        ))
    }
}
/// Load through the existing configuration pipeline with Unix nonblocking,
/// same-descriptor regular-file reads: 8 MiB per file, 32 MiB per invocation.
pub fn load_project_bounded(
    home: &Path,
    cwd: &Path,
    env: &Env,
    project: Option<&str>,
) -> Result<Config, Error> {
    load(home, cwd, env, project, None)
}
/// Native Metadata already carries the process-cwd base lazily. Preserve
/// that behavior: absolute project inputs must not query a deleted cwd.
pub(crate) fn load_project_bounded_lazy(
    home: &Path,
    cwd: &Path,
    env: &Env,
    project: Option<&str>,
) -> Result<Config, Error> {
    #[cfg(unix)]
    {
        unix::load_with_limits_at(home, cwd, env, project, None, FILE_LIMIT, TOTAL_LIMIT)
    }
    #[cfg(not(unix))]
    {
        load(home, cwd, env, project, None)
    }
}
/// The snapshot counterpart of `load_project_bounded`, sharing one IO budget
/// and exactly the existing snapshot context and configuration pipeline.
pub fn load_snapshot_bounded(
    home: &Path,
    cwd: &Path,
    env: &Env,
    project: Option<&str>,
    version: f64,
    loaded_at: &str,
) -> Result<JsValue, Error> {
    Ok(
        load(home, cwd, env, project, Some((version, loaded_at.into())))?
            .snapshot
            .expect("snapshot context records a snapshot"),
    )
}
#[cfg(not(unix))]
fn load(
    _: &Path,
    _: &Path,
    _: &Env,
    _: Option<&str>,
    _: Option<(f64, JsString)>,
) -> Result<Config, Error> {
    Err(Error::new(
        "CONFIG_BOUNDED_UNSUPPORTED",
        "Bounded configuration loading requires Unix file descriptors.",
    ))
}
#[cfg(unix)]
fn load(
    home: &Path,
    cwd: &Path,
    env: &Env,
    project: Option<&str>,
    context: Option<(f64, JsString)>,
) -> Result<Config, Error> {
    unix::load_with_limits(home, cwd, env, project, context, FILE_LIMIT, TOTAL_LIMIT)
}
#[cfg(unix)]
mod unix {
    use super::*;
    use crate::metadata::{Files, Metadata, NativeFiles, Stat};
    use std::{
        cell::Cell,
        fs::OpenOptions,
        io::{self, Read},
        os::unix::fs::OpenOptionsExt,
    };

    pub(super) fn binding_target(
        home: &Path,
        env: &Env,
        project: &str,
    ) -> Result<Option<JsString>, Error> {
        let metadata = Metadata {
            home: home.to_path_buf(),
            cwd: Path::new("").to_path_buf(),
            files: BoundedFiles::new(FILE_LIMIT, TOTAL_LIMIT),
        };
        let result = super::super::binding::target_agent(&metadata, project, env);
        match metadata.files.error() {
            Some(error) => Err(error),
            None => result,
        }
    }
    #[derive(Clone, Copy)]
    enum Fault {
        NonRegular,
        FileLimit,
        TotalLimit,
    }
    struct BoundedFiles {
        native: NativeFiles,
        used: Cell<usize>,
        fault: Cell<Option<Fault>>,
        file_limit: usize,
        total_limit: usize,
    }
    impl BoundedFiles {
        fn new(file_limit: usize, total_limit: usize) -> Self {
            Self {
                native: NativeFiles,
                used: Cell::new(0),
                fault: Cell::new(None),
                file_limit,
                total_limit,
            }
        }
        fn refusal(&self, fault: Fault) -> io::Error {
            if self.fault.get().is_none() {
                self.fault.set(Some(fault));
            }
            Self::marker()
        }
        fn marker() -> io::Error {
            io::Error::other("bounded configuration read refused")
        }
        fn active(&self) -> io::Result<()> {
            if self.fault.get().is_some() {
                Err(Self::marker())
            } else {
                Ok(())
            }
        }
        fn error(&self) -> Option<Error> {
            self.fault.get().map(|fault| match fault {
                Fault::NonRegular => Error::new(
                    "CONFIG_READ_NOT_REGULAR",
                    "Configuration input must be a regular file.",
                ),
                Fault::FileLimit => Error::new(
                    "CONFIG_READ_LIMIT",
                    format!(
                        "Configuration input exceeds the {}-byte limit.",
                        self.file_limit
                    ),
                ),
                Fault::TotalLimit => Error::new(
                    "CONFIG_READ_BUDGET",
                    format!(
                        "Configuration inputs exceed the {}-byte total read budget.",
                        self.total_limit
                    ),
                ),
            })
        }
    }
    impl Files for BoundedFiles {
        fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
            self.active()?;
            self.native.entries(path)
        }
        fn stat(&self, path: &Path) -> io::Result<Stat> {
            self.active()?;
            self.native.stat(path)
        }
        fn process_alive(&self, pid: i32) -> bool {
            self.fault.get().is_none() && self.native.process_alive(pid)
        }
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.active()?;
            let remaining = self.total_limit.saturating_sub(self.used.get());
            if remaining == 0 {
                return Err(self.refusal(Fault::TotalLimit));
            }
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?;
            let metadata = file
                .metadata()
                .map_err(|error| crate::node_io::descriptor_error(error, "fstat"))?;
            if !metadata.is_file() {
                return Err(self.refusal(Fault::NonRegular));
            }
            if metadata.len() > self.file_limit as u64 {
                return Err(self.refusal(Fault::FileLimit));
            }
            if metadata.len() > remaining as u64 {
                return Err(self.refusal(Fault::TotalLimit));
            }
            // Charge actual bytes, including partial reads and the one-byte
            // growth probe. No later operation delegates after a typed fault.
            let mut bytes = Vec::new();
            let result = file
                .take((self.file_limit.min(remaining) + 1) as u64)
                .read_to_end(&mut bytes);
            self.used.set(self.used.get().saturating_add(bytes.len()));
            if bytes.len() > self.file_limit {
                return Err(self.refusal(Fault::FileLimit));
            }
            if bytes.len() > remaining {
                return Err(self.refusal(Fault::TotalLimit));
            }
            result.map_err(|error| crate::node_io::descriptor_error(error, "read"))?;
            Ok(bytes)
        }
    }
    pub(super) fn load_with_limits(
        home: &Path,
        cwd: &Path,
        env: &Env,
        project: Option<&str>,
        context: Option<(f64, JsString)>,
        file_limit: usize,
        total_limit: usize,
    ) -> Result<Config, Error> {
        let cwd = super::super::absolute_cwd(cwd)?;
        load_with_limits_at(home, &cwd, env, project, context, file_limit, total_limit)
    }

    pub(super) fn load_with_limits_at(
        home: &Path,
        cwd: &Path,
        env: &Env,
        project: Option<&str>,
        context: Option<(f64, JsString)>,
        file_limit: usize,
        total_limit: usize,
    ) -> Result<Config, Error> {
        let metadata = Metadata {
            home: home.to_path_buf(),
            cwd: cwd.to_path_buf(),
            files: BoundedFiles::new(file_limit, total_limit),
        };
        let result = super::super::load_project_with_files_inner(&metadata, env, project, context);
        // A resource rejection has its own precise contract. Ordinary IO never
        // records a fault and keeps the original loader's diagnostic ordering.
        match metadata.files.error() {
            Some(error) => Err(error),
            None => result,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            fs,
            path::PathBuf,
            process::{Child, Command, Stdio},
            time::{Duration, Instant},
        };
        const YAML: &str = "schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: https://fixture.invalid/v1\n      apiKey: mock-only\n      models: {mock: {}}\n";
        struct Scratch(PathBuf);
        impl Scratch {
            fn new() -> Self {
                let root = std::env::temp_dir()
                    .join(format!("pdrs-bounded-config-{}", uuid::Uuid::new_v4()));
                fs::create_dir(&root).unwrap();
                let root = fs::canonicalize(root).unwrap();
                fs::create_dir(root.join("home")).unwrap();
                fs::create_dir(root.join("project")).unwrap();
                fs::write(root.join("home/pilotdeck.yaml"), YAML).unwrap();
                Self(root)
            }
        }
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        #[test]
        fn bounded_snapshot_matches_the_existing_snapshot_for_regular_inputs() {
            let scratch = Scratch::new();
            let home = scratch.0.join("home");
            let cwd = scratch.0.join("project");
            let env = Env::new();
            let before = fs::read(home.join("pilotdeck.yaml")).unwrap();
            let expected = super::super::super::load_snapshot(
                &home,
                &cwd,
                &env,
                Some("."),
                1.0,
                "2026-01-01T00:00:00.000Z",
            )
            .unwrap();
            let actual = load_snapshot_bounded(
                &home,
                &cwd,
                &env,
                Some("."),
                1.0,
                "2026-01-01T00:00:00.000Z",
            )
            .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(fs::read(home.join("pilotdeck.yaml")).unwrap(), before);
            assert!(load_project_bounded(&home, &cwd, &env, Some(".")).is_ok());
        }
        #[test]
        fn cumulative_actual_reads_exhaust_one_budget_and_stop_all_further_io() {
            let scratch = Scratch::new();
            let file = scratch.0.join("sparse");
            fs::File::create(&file)
                .unwrap()
                .set_len(FILE_LIMIT as u64)
                .unwrap();
            let files = BoundedFiles::new(FILE_LIMIT, TOTAL_LIMIT);
            for _ in 0..4 {
                assert_eq!(files.read(&file).unwrap().len(), FILE_LIMIT);
            }
            assert_eq!(files.used.get(), TOTAL_LIMIT);
            assert!(files.read(&file).is_err());
            assert_eq!(files.error().unwrap().code, "CONFIG_READ_BUDGET");
            let missing = scratch.0.join("missing");
            assert_eq!(
                files.read(&missing).unwrap_err().kind(),
                io::ErrorKind::Other
            );
            assert_eq!(
                files.stat(&missing).err().unwrap().kind(),
                io::ErrorKind::Other
            );
            assert_eq!(
                files.entries(&missing).unwrap_err().kind(),
                io::ErrorKind::Other
            );
            assert!(!files.process_alive(std::process::id() as i32));
            assert_eq!(files.used.get(), TOTAL_LIMIT);
        }
        #[test]
        #[ignore = "deadline-guarded child from bounded_loader_resource_inputs_are_rejected_without_changes"]
        fn bounded_resource_child() {
            let root = PathBuf::from(std::env::var_os("PDRS_CONFIG_BOUNDED_ROOT").unwrap());
            let phase = std::env::var("PDRS_CONFIG_BOUNDED_PHASE").unwrap();
            let home = root.join("home");
            let cwd = root.join("project");
            let env = Env::from([
                ("STAFFDECK_PROJECT_BINDING_ENABLED".into(), "true".into()),
                ("STAFFDECK_COPY_TENANT_ID".into(), "fixture".into()),
                (
                    "STAFFDECK_FORMAL_API_ORIGIN".into(),
                    "https://fixture.invalid".into(),
                ),
            ]);
            for snapshot in [false, true] {
                let error = if snapshot {
                    load_snapshot_bounded(
                        &home,
                        &cwd,
                        &env,
                        Some("."),
                        1.0,
                        "2026-01-01T00:00:00.000Z",
                    )
                    .err()
                    .unwrap()
                } else {
                    load_project_bounded(&home, &cwd, &env, Some("."))
                        .err()
                        .unwrap()
                };
                assert!(!error.requires_host);
                assert_eq!(
                    error.code,
                    if phase == "oversize" {
                        "CONFIG_READ_LIMIT"
                    } else {
                        "CONFIG_READ_NOT_REGULAR"
                    }
                );
                assert_eq!(
                    error.message.to_utf8().unwrap(),
                    if phase == "oversize" {
                        "Configuration input exceeds the 8388608-byte limit."
                    } else {
                        "Configuration input must be a regular file."
                    }
                );
            }
            println!("CONFIG_RESOURCE_REJECTED={phase}");
        }
        struct OwnedChild(Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        #[test]
        fn bounded_loader_resource_inputs_are_rejected_without_changes() {
            use std::{io::Read, os::unix::fs::MetadataExt};
            for phase in [
                "global-fifo",
                "store-fifo",
                "sop-fifo",
                "lock-fifo",
                "oversize",
            ] {
                let scratch = Scratch::new();
                let home = scratch.0.join("home");
                fs::create_dir(home.join("staffdeck")).unwrap();
                let target = match phase {
                    "store-fifo" => home.join("staffdeck/project-bindings.json"),
                    "sop-fifo" => home.join("staffdeck/fixture-binding/sops.json"),
                    "lock-fifo" => home.join("staffdeck/project-bindings.json.lock"),
                    _ => home.join("pilotdeck.yaml"),
                };
                if matches!(phase, "sop-fifo" | "lock-fifo") {
                    fs::create_dir(home.join("staffdeck/fixture-binding")).unwrap();
                    let store = serde_json::json!({"version":1,"installationId":"fixture","projects":[{"projectRoot":scratch.0.join("project"),"bindingId":"fixture-binding","status":if phase=="lock-fifo" {"binding"}else {"bound"},"runtimeReady":true,"origin":"https://fixture.invalid","tenantId":"fixture","agentId":"fixture"}]});
                    fs::write(
                        home.join("staffdeck/project-bindings.json"),
                        store.to_string(),
                    )
                    .unwrap();
                }
                let before_yaml = fs::read(home.join("pilotdeck.yaml")).unwrap();
                if target.exists() {
                    fs::remove_file(&target).unwrap();
                }
                if phase == "oversize" {
                    fs::File::create(&target)
                        .unwrap()
                        .set_len((FILE_LIMIT + 1) as u64)
                        .unwrap();
                } else {
                    assert!(
                        Command::new("/usr/bin/mkfifo")
                            .arg(&target)
                            .status()
                            .unwrap()
                            .success()
                    );
                }
                let before = fs::symlink_metadata(&target).unwrap();
                let name = concat!(module_path!(), "::bounded_resource_child")
                    .split_once("::")
                    .unwrap()
                    .1;
                let mut child = OwnedChild(
                    Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", name, "--ignored", "--nocapture"])
                        .env("PDRS_CONFIG_BOUNDED_ROOT", &scratch.0)
                        .env("PDRS_CONFIG_BOUNDED_PHASE", phase)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .unwrap(),
                );
                let deadline = Instant::now() + Duration::from_secs(5);
                while child.0.try_wait().unwrap().is_none() {
                    assert!(
                        Instant::now() < deadline,
                        "bounded config child exceeded five seconds"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                let mut stdout = String::new();
                let mut stderr = String::new();
                child
                    .0
                    .stdout
                    .take()
                    .unwrap()
                    .read_to_string(&mut stdout)
                    .unwrap();
                child
                    .0
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .unwrap();
                assert!(child.0.wait().unwrap().success(), "{stdout}{stderr}");
                assert!(
                    stdout
                        .lines()
                        .any(|line| line == format!("CONFIG_RESOURCE_REJECTED={phase}"))
                );
                let after = fs::symlink_metadata(&target).unwrap();
                assert_eq!(
                    (before.ino(), before.mode(), before.len()),
                    (after.ino(), after.mode(), after.len())
                );
                if phase == "oversize" {
                    assert!(fs::read(&target).unwrap().iter().all(|byte| *byte == 0));
                }
                if !matches!(phase, "global-fifo" | "oversize") {
                    assert_eq!(fs::read(home.join("pilotdeck.yaml")).unwrap(), before_yaml);
                }
                assert_eq!(fs::read_dir(scratch.0.join("project")).unwrap().count(), 0);
            }
        }
    }
}
