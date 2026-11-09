//! Explicit foreground entry for the existing native Gateway composition.
//! Host ownership and shutdown stay in native_process.
use pilotdeck_gateway::ProxyError;
#[cfg(unix)]
use pilotdeck_gateway::native_process;
use std::ffi::OsString;

pub(super) const HELP: &str = "pilotdeck-control serve --home <dir> --project <dir> --listen <127.0.0.1:port> --token-file <file> --sidecar <executable>
  [--sidecar-arg <value>]... [--drain-ms <1..300000>] [--storage-root <dir>]

Runs the existing native Gateway in the foreground on Unix with /dev/fd support.
All authorities are explicit. The token input must be a regular file of at most 64 KiB.
Home and project must exist and be separate directories. The listen port must be 0 or at least 1024.
Readiness and stopping diagnostics go to stderr; stdout has no JSON readiness protocol.
Use a data root owned by this host. This entry does not discover or stop another host.
The sidecar executable must already be installed and explicitly selected.
This entry does not implement the new app JSON-RPC server or automatic host installation.";

#[cfg(unix)]
pub(super) fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), ProxyError> {
    // The native parser validates authorities before any host is started. Its
    // later token reopen must consume our private bounded snapshot, not reopen
    // the caller's path after a separate CLI preflight.
    let options = native_process::Options::parse(args)?;
    let runtime = runtime()?;
    let snapshot = token::Snapshot::read(&options.token_file)?;
    run_with_runtime(runtime, options, snapshot)
}
#[cfg(unix)]
fn runtime() -> Result<tokio::runtime::Runtime, ProxyError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // Install the native write-stage disposition before snapshot.write_all,
    // so a real EFBIG returns an error and runs the cleanup guard.
    runtime.block_on(async {
        pilotdeck_gateway::ensure_sigxfsz_ignored();
    });
    Ok(runtime)
}
#[cfg(all(unix, test))]
fn run_snapshot(
    options: native_process::Options,
    snapshot: token::Snapshot,
) -> Result<(), ProxyError> {
    run_with_runtime(runtime()?, options, snapshot)
}
#[cfg(unix)]
fn run_with_runtime(
    runtime: tokio::runtime::Runtime,
    mut options: native_process::Options,
    snapshot: token::Snapshot,
) -> Result<(), ProxyError> {
    options.token_file = snapshot.path();
    let result = runtime.block_on(native_process::run(options));
    drop(snapshot);
    result
}
#[cfg(not(unix))]
pub(super) fn run(_args: impl IntoIterator<Item = OsString>) -> Result<(), ProxyError> {
    Err("serve requires Unix /dev/fd token isolation".into())
}

#[cfg(unix)]
mod token {
    use super::ProxyError;
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::{
        ffi::OsString,
        fs::{File, OpenOptions},
        io::{Seek, SeekFrom, Write},
        os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        },
        path::{Path, PathBuf},
    };

    fn failure() -> ProxyError {
        "Cannot create private token snapshot.".into()
    }
    struct Directory {
        parent: File,
        directory: File,
        name: OsString,
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            // Only our fixed child name and exclusive directory are removed.
            let _ = rustix::fs::unlinkat(&self.directory, "token", AtFlags::empty());
            let _ = rustix::fs::unlinkat(&self.parent, &self.name, AtFlags::REMOVEDIR);
        }
    }
    pub(super) struct Snapshot {
        _directory: Directory,
        file: File,
    }
    impl Snapshot {
        pub(super) fn read(path: &Path) -> Result<Self, ProxyError> {
            let bytes = super::super::read_regular(path, super::super::MAX_TOKEN)
                .map_err(|error| error.message)?;
            Self::new(&bytes)
        }
        fn new(bytes: &[u8]) -> Result<Self, ProxyError> {
            // A fixed root-owned /tmp avoids trusting a caller-controlled
            // TMPDIR ancestor. Sticky writable parents protect our entry from
            // other UIDs; root and same-UID debuggers are outside this boundary.
            let parent = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open("/tmp")
                .map_err(|_| failure())?;
            let metadata = parent.metadata().map_err(|_| failure())?;
            if metadata.uid() != 0
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                return Err(failure());
            }
            let name = OsString::from(format!(
                "pdrs-control-token-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            rustix::fs::mkdirat(&parent, &name, Mode::from_raw_mode(0o700))
                .map_err(|_| failure())?;
            let directory = match rustix::fs::openat(
                &parent,
                &name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(directory) => File::from(directory),
                Err(_) => {
                    let _ = rustix::fs::unlinkat(&parent, &name, AtFlags::REMOVEDIR);
                    return Err(failure());
                }
            };
            let directory = Directory {
                parent,
                directory,
                name,
            };
            let mode = directory
                .directory
                .metadata()
                .map_err(|_| failure())?
                .mode()
                & 0o777;
            if mode != 0o700 {
                return Err(failure());
            }
            let mut attributes = [0u8; 1];
            if !matches!(
                rustix::fs::flistxattr(&directory.directory, &mut attributes[..]),
                Ok(0)
            ) {
                return Err(failure());
            }
            #[cfg(target_os = "macos")]
            {
                // On macOS ACLs are distinct from xattrs. Inspect the empty
                // directory before any token fd exists. Its root/sticky parent
                // prevents another UID from replacing this directory entry.
                let output = std::process::Command::new("/bin/ls")
                    .args(["-ledn", "--"])
                    .arg(Path::new("/tmp").join(&directory.name))
                    .env("LC_ALL", "C")
                    .output()
                    .map_err(|_| failure())?;
                let text = String::from_utf8_lossy(&output.stdout);
                let mode = text.split_whitespace().next().ok_or_else(failure)?;
                if !output.status.success() || mode.len() < 10 || mode.contains('+') {
                    return Err(failure());
                }
            }
            let fd = rustix::fs::openat(
                &directory.directory,
                "token",
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| failure())?;
            let mut file = File::from(fd);
            // Unlink before writing even one secret byte. All later native
            // reads refer to this exact anonymous inode through our fd table.
            rustix::fs::unlinkat(&directory.directory, "token", AtFlags::empty())
                .map_err(|_| failure())?;
            let metadata = file.metadata().map_err(|_| failure())?;
            if metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 0 {
                return Err(failure());
            }
            file.write_all(bytes).map_err(|_| failure())?;
            file.seek(SeekFrom::Start(0)).map_err(|_| failure())?;
            let mut snapshot = Self {
                _directory: directory,
                file,
            };
            // Prove /dev/fd actually reopens this file before starting a host.
            // macOS shares the cursor; always reset it after the positive probe.
            let probe = super::super::read_regular(&snapshot.path(), super::super::MAX_TOKEN)
                .map_err(|_| failure())?;
            if probe != bytes {
                return Err(failure());
            }
            snapshot
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|_| failure())?;
            Ok(snapshot)
        }
        pub(super) fn path(&self) -> PathBuf {
            PathBuf::from(format!("/dev/fd/{}", self.file.as_raw_fd()))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs;
        #[test]
        fn anonymous_snapshot_is_private_unlinked_cloexec_and_reopens_exact_bytes() {
            let bytes = b"fixture-token\r\n";
            let snapshot = Snapshot::new(bytes).unwrap();
            assert_eq!(
                snapshot._directory.directory.metadata().unwrap().mode() & 0o777,
                0o700
            );
            assert_eq!(snapshot.file.metadata().unwrap().mode() & 0o777, 0o600);
            assert_eq!(snapshot.file.metadata().unwrap().nlink(), 0);
            assert!(
                rustix::io::fcntl_getfd(&snapshot.file)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            assert!(
                rustix::io::fcntl_getfd(&snapshot._directory.directory)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
            let directory = Path::new("/tmp").join(&snapshot._directory.name);
            assert!(!directory.join("token").exists());
            assert_eq!(fs::read(snapshot.path()).unwrap(), bytes);
            drop(snapshot);
            assert!(!directory.exists());
        }
        fn args(root: &Path) -> Vec<OsString> {
            let mut args = Vec::new();
            for (key, value) in [
                ("--home", root.join("home")),
                ("--project", root.join("project")),
                ("--token-file", root.join("token")),
                ("--sidecar", root.join("sidecar")),
            ] {
                args.push(OsString::from(key));
                args.push(value.into_os_string());
            }
            args.extend([OsString::from("--listen"), OsString::from("127.0.0.1:0")]);
            args
        }
        fn options(root: &Path) -> super::super::native_process::Options {
            super::super::native_process::Options::parse(args(root)).unwrap()
        }
        struct Scratch(PathBuf);
        impl Scratch {
            fn new() -> Self {
                let root = fs::canonicalize(std::env::temp_dir())
                    .unwrap()
                    .join(format!("pdrs-serve-token-test-{}", uuid::Uuid::new_v4()));
                fs::create_dir(&root).unwrap();
                for name in ["home", "project"] {
                    fs::create_dir(root.join(name)).unwrap();
                }
                fs::write(root.join("token"), b"fixture-token\r\n").unwrap();
                fs::write(root.join("sidecar"), b"unused parse fixture").unwrap();
                Self(root)
            }
        }
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        #[test]
        fn token_snapshot_is_cleaned_when_native_startup_fails() {
            let scratch = Scratch::new();
            fs::write(scratch.0.join("home/pilotdeck.yaml"), b"schemaVersion: [").unwrap();
            for bytes in [b"fixture-token\r\n".as_slice(), b" \r\n".as_slice()] {
                fs::write(scratch.0.join("token"), bytes).unwrap();
                let options = options(&scratch.0);
                let snapshot = Snapshot::read(&options.token_file).unwrap();
                let directory = Path::new("/tmp").join(&snapshot._directory.name);
                let error = super::super::run_snapshot(options, snapshot).unwrap_err();
                if bytes == b" \r\n" {
                    assert_eq!(
                        error.to_string(),
                        "native token file must contain a nonempty token"
                    );
                } else {
                    assert_ne!(
                        error.to_string(),
                        "native token file must contain a nonempty token"
                    );
                }
                assert!(!directory.exists());
                assert_eq!(fs::read(scratch.0.join("token")).unwrap(), bytes);
            }
        }
        #[test]
        #[ignore = "deadline-guarded subprocess from token_path_replacements_and_oversize_are_bounded"]
        fn original_token_race_child() {
            let root = PathBuf::from(std::env::var_os("PDRS_SERVE_TOKEN_ROOT").unwrap());
            let phase = std::env::var("PDRS_SERVE_TOKEN_PHASE").unwrap();
            if phase == "fsize" {
                let error = super::super::run(args(&root)).unwrap_err();
                assert_eq!(error.to_string(), "Cannot create private token snapshot.");
                let prefix = format!("pdrs-control-token-{}-", std::process::id());
                assert!(!fs::read_dir("/tmp").unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&prefix)
                }));
                assert_eq!(fs::read(root.join("token")).unwrap(), b"fixture-token\r\n");
                println!("TOKEN_RACE_BOUNDED={phase}");
                return;
            }
            if phase == "after" {
                fs::write(root.join("token"), b"\xff").unwrap();
            }
            if phase == "oversize" {
                File::options()
                    .write(true)
                    .open(root.join("token"))
                    .unwrap()
                    .set_len((super::super::super::MAX_TOKEN + 1) as u64)
                    .unwrap();
            }
            let options = options(&root);
            let snapshot = if phase == "after" {
                Some(Snapshot::read(&options.token_file).unwrap())
            } else {
                None
            };
            if phase != "oversize" {
                fs::remove_file(root.join("token")).unwrap();
                assert!(
                    std::process::Command::new("/usr/bin/mkfifo")
                        .arg(root.join("token"))
                        .status()
                        .unwrap()
                        .success()
                );
            }
            if let Some(snapshot) = snapshot {
                let directory = Path::new("/tmp").join(&snapshot._directory.name);
                // The actual native read must consume the anonymous raw bytes,
                // producing UTF-8 failure instead of blocking on the new FIFO.
                let error = super::super::run_snapshot(options, snapshot).unwrap_err();
                assert_eq!(error.to_string(), "stream did not contain valid UTF-8");
                assert!(!directory.exists());
            } else {
                let error = Snapshot::read(&options.token_file)
                    .err()
                    .expect("unsafe original rejected");
                assert_eq!(
                    error.to_string(),
                    if phase == "oversize" {
                        "Input exceeds the size limit."
                    } else {
                        "Input must be a regular file."
                    }
                );
            }
            println!("TOKEN_RACE_BOUNDED={phase}");
        }
        #[test]
        fn token_path_replacements_and_oversize_are_bounded() {
            use std::{
                process::{Command, Stdio},
                time::{Duration, Instant},
            };
            struct Child(std::process::Child);
            impl Drop for Child {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            for phase in ["before", "after", "oversize", "fsize"] {
                let scratch = Scratch::new();
                let test = concat!(module_path!(), "::original_token_race_child")
                    .split_once("::")
                    .unwrap()
                    .1;
                let mut command = if phase == "fsize" {
                    let mut command = Command::new("/bin/sh");
                    command.arg("-c").arg("ulimit -f 0 || exit 1; exec \"$0\" --exact \"$1\" --ignored --nocapture")
                        .arg(std::env::current_exe().unwrap()).arg(test);
                    command
                } else {
                    let mut command = Command::new(std::env::current_exe().unwrap());
                    command.args(["--exact", test, "--ignored", "--nocapture"]);
                    command
                };
                let mut child = Child(
                    command
                        .env("PDRS_SERVE_TOKEN_ROOT", &scratch.0)
                        .env("PDRS_SERVE_TOKEN_PHASE", phase)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .unwrap(),
                );
                let deadline = Instant::now() + Duration::from_secs(5);
                while child.0.try_wait().unwrap().is_none() {
                    assert!(
                        Instant::now() < deadline,
                        "token race child exceeded five seconds"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                use std::io::Read;
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
                        .any(|line| line == format!("TOKEN_RACE_BOUNDED={phase}"))
                );
                for name in ["home", "project"] {
                    assert_eq!(fs::read_dir(scratch.0.join(name)).unwrap().count(), 0);
                }
            }
        }
    }
}
