//! Bounded updater subprocesses. The leader remains waitable until its final
//! group signal, preventing reuse of its PGID while cleanup is in progress.
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use std::{
    io::Read,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub struct Signals {
    pub cancelled: Arc<AtomicBool>,
    ids: Vec<signal_hook::SigId>,
}
impl Signals {
    pub fn install() -> Result<Self, String> {
        let mut value = Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            ids: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            value.ids.push(
                signal_hook::flag::register(signal, value.cancelled.clone())
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(value)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}
pub struct Runner<'a> {
    pub timeout: Duration,
    pub output_limit: usize,
    pub cancelled: Option<&'a AtomicBool>,
}
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
    pub success: bool,
}
pub(super) struct Progress {
    stdout: std::io::Stdout,
    original: rustix::fs::OFlags,
}
impl Progress {
    pub(super) fn new() -> Result<Self, String> {
        let stdout = std::io::stdout();
        let original = rustix::fs::fcntl_getfl(&stdout)
            .map_err(|e| format!("Cannot inspect update progress output: {e}"))?;
        rustix::fs::fcntl_setfl(&stdout, original | rustix::fs::OFlags::NONBLOCK)
            .map_err(|e| format!("Cannot make update progress output nonblocking: {e}"))?;
        Ok(Self { stdout, original })
    }
    pub(super) fn write(&self, bytes: &[u8]) -> Result<(), String> {
        // A partial write and EAGAIN drop only progress. The complete bounded
        // diagnostic tail is retained separately by the reader below.
        match rustix::io::write(&self.stdout, bytes) {
            Ok(_) | Err(rustix::io::Errno::AGAIN) | Err(rustix::io::Errno::INTR) => Ok(()),
            Err(e) => Err(format!("Update progress output failed: {e}")),
        }
    }
    pub(super) fn restore(&self) -> Result<(), String> {
        rustix::fs::fcntl_setfl(&self.stdout, self.original)
            .map_err(|e| format!("Cannot restore update progress output flags: {e}"))
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

struct ChildGuard {
    child: std::process::Child,
    group: Pid,
    reaped: bool,
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = rustix::process::kill_process_group(self.group, Signal::KILL);
            // Drop must not park forever on an uninterruptible child. The
            // caller retains staging/lock on any unconfirmed termination.
            if exited(self.group).unwrap_or(false) {
                let _ = self.child.wait();
            }
        }
    }
}
fn exited(group: Pid) -> Result<bool, String> {
    match rustix::process::waitid(
        WaitId::Pid(group),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    ) {
        Ok(status) => Ok(status.is_some()),
        Err(rustix::io::Errno::INTR) => Ok(false),
        Err(e) => Err(format!("Unable to confirm update child identity: {e}")),
    }
}

#[cfg(any(target_os = "macos", test))]
fn listing_is_zombie_only(text: &str, leader: u32) -> bool {
    let mut saw_leader = false;
    let mut count = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 3 {
            return false;
        }
        let Ok(pid) = fields[0].parse::<u32>() else {
            return false;
        };
        let Ok(group) = fields[1].parse::<u32>() else {
            return false;
        };
        if pid == 0 || group != leader || !fields[2].starts_with('Z') {
            return false;
        }
        saw_leader |= pid == leader;
        count += 1;
    }
    saw_leader && count > 0
}

/// Darwin returns EPERM for a group containing only zombies. It also returns
/// EPERM for living members we cannot signal, so the errno alone is not proof.
/// Inspect this exact reserved group with the system ps. Failure, truncation,
/// malformed rows, a missing leader or any non-zombie member fail closed.
#[cfg(target_os = "macos")]
fn darwin_zombie_only_group(leader: Pid, raw: u32) -> Result<bool, String> {
    struct Inspection {
        child: std::process::Child,
        pid: Pid,
        reaped: bool,
    }
    impl Drop for Inspection {
        fn drop(&mut self) {
            if !self.reaped {
                let _ = self.child.kill();
                if exited(self.pid).unwrap_or(false) {
                    let _ = self.child.wait();
                }
            }
        }
    }
    let child = Command::new("/bin/ps")
        .args(["-g", &raw.to_string(), "-o", "pid=,pgid=,stat="])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Update process membership inspection failed: {e}"))?;
    let pid = Pid::from_raw(child.id() as i32).ok_or("Invalid inspection process identity.")?;
    let mut inspection = Inspection {
        child,
        pid,
        reaped: false,
    };
    let mut stdout = inspection
        .child
        .stdout
        .take()
        .ok_or("Inspection stdout unavailable.")?;
    let flags = rustix::fs::fcntl_getfl(&stdout).map_err(|e| e.to_string())?;
    rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        for _ in 0..16 {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if bytes.len() + n > 65536 {
                        return Err(
                            "Update process membership inspection exceeded its limit.".into()
                        );
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("Update process membership inspection failed: {e}")),
            }
        }
        if exited(pid)? {
            // ps cannot have a writer after its observed exit; drain its small
            // remaining pipe once more before validating the complete listing.
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if bytes.len() + n > 65536 {
                            return Err(
                                "Update process membership inspection exceeded its limit.".into()
                            );
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        return Err(format!("Update process membership inspection failed: {e}"));
                    }
                }
            }
            let status = inspection.child.wait().map_err(|e| e.to_string())?;
            inspection.reaped = true;
            return Ok(status.success()
                && exited(leader)?
                && std::str::from_utf8(&bytes)
                    .is_ok_and(|text| listing_is_zombie_only(text, raw)));
        }
        if Instant::now() >= deadline {
            return Err("Update process membership inspection timed out.".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn stop_group(guard: &ChildGuard) -> Result<(), String> {
    confirm_group_signal(
        guard,
        rustix::process::kill_process_group(guard.group, Signal::KILL),
    )
}
fn confirm_group_signal(
    guard: &ChildGuard,
    result: Result<(), rustix::io::Errno>,
) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    let _ = guard;
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            #[cfg(target_os = "macos")]
            if error == rustix::io::Errno::PERM
                && exited(guard.group)?
                && darwin_zombie_only_group(guard.group, guard.child.id())?
            {
                return Ok(());
            }
            Err(format!(
                "Update command termination could not be confirmed: {error}"
            ))
        }
    }
}

impl Runner<'_> {
    /// Output is a bounded diagnostic tail. Pipes are read without waiting for
    /// EOF: a grandchild holding an inherited pipe cannot defeat the deadline.
    pub fn run(&self, command: &mut Command, progress: bool) -> Result<Output, String> {
        if self.cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err("Update was cancelled.".into());
        }
        let progress = if progress {
            Some(Progress::new()?)
        } else {
            None
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command
            .spawn()
            .map_err(|e| format!("Unable to start update command: {e}"))?;
        let group =
            Pid::from_raw(child.id() as i32).ok_or("Invalid update child process identity.")?;
        let mut guard = ChildGuard {
            child,
            group,
            reaped: false,
        };
        let mut stdout = guard
            .child
            .stdout
            .take()
            .ok_or("Update stdout pipe unavailable.")?;
        let mut stderr = guard
            .child
            .stderr
            .take()
            .ok_or("Update stderr pipe unavailable.")?;
        for fd in [
            &stdout as &dyn std::os::fd::AsFd,
            &stderr as &dyn std::os::fd::AsFd,
        ] {
            let flags = rustix::fs::fcntl_getfl(fd).map_err(|e| e.to_string())?;
            rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
                .map_err(|e| e.to_string())?;
        }
        let deadline = Instant::now() + self.timeout;
        let mut out_tail = Vec::new();
        let mut err_tail = Vec::new();
        let mut truncated = false;
        let progress_failed = std::cell::Cell::new(false);
        let mut capture = |reader: &mut dyn Read, tail: &mut Vec<u8>| -> Result<(), String> {
            let mut buf = [0_u8; 8192];
            // One bounded batch per pipe per turn, even for infinite writers.
            for _ in 0..16 {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Some(progress) = &progress
                            && !progress_failed.get()
                            && progress.write(&buf[..n]).is_err()
                        {
                            progress_failed.set(true);
                            break;
                        }
                        if n >= self.output_limit {
                            truncated |= tail.len() + n > self.output_limit;
                            tail.clear();
                            tail.extend_from_slice(&buf[n - self.output_limit..n]);
                        } else {
                            let discard = (tail.len() + n).saturating_sub(self.output_limit);
                            truncated |= discard > 0;
                            tail.drain(..discard);
                            tail.extend_from_slice(&buf[..n]);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(())
        };
        let interrupted = loop {
            capture(&mut stdout, &mut out_tail)?;
            capture(&mut stderr, &mut err_tail)?;
            if progress_failed.get() {
                break Some("Update was cancelled.");
            }
            if exited(group)? {
                break None;
            }
            if self.cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
                break Some("Update was cancelled.");
            }
            if Instant::now() >= deadline {
                break Some("Update command timed out.");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Only signal while the leader still reserves the PID. Never signal
        // after Child::wait has reaped it. Descendants are not allowed to keep
        // running after a completed updater command either.
        stop_group(&guard)?;
        let cleanup_deadline = Instant::now() + Duration::from_secs(3);
        while !exited(group)? {
            if Instant::now() >= cleanup_deadline {
                return Err("Update command termination could not be confirmed; retain the update lock and staging directory.".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = guard.child.wait().map_err(|e| e.to_string())?;
        guard.reaped = true;
        capture(&mut stdout, &mut out_tail)?;
        capture(&mut stderr, &mut err_tail)?;
        let output = Output {
            stdout: String::from_utf8_lossy(&out_tail).trim().to_owned(),
            stderr: String::from_utf8_lossy(&err_tail).trim().to_owned(),
            truncated,
            exit_code: status.code(),
            success: status.success(),
        };
        if let Some(progress) = &progress {
            progress.restore()?;
        }
        if let Some(message) =
            interrupted.or_else(|| progress_failed.get().then_some("Update was cancelled."))
        {
            return Err(message.into());
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(script: &str) -> Command {
        let mut c = Command::new("/bin/sh");
        c.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/nonexistent-pilotdeck-update-test-home")
            .arg("-c")
            .arg(script);
        c
    }
    #[test]
    fn zombie_exception_requires_the_complete_same_group_listing() {
        assert!(listing_is_zombie_only("17 17 Z\n18 17 Z+\n", 17));
        for text in [
            "",
            "17 17 S\n",
            "17 17 Z\n18 17 S\n",
            "18 17 Z\n",
            "17 19 Z\n",
            "17 17 Z extra\n",
            "garbage",
            "17 17 Z\n18 17 T\n",
        ] {
            assert!(!listing_is_zombie_only(text, 17), "{text}");
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn live_descendant_never_qualifies_for_darwin_eperm_exception() {
        let mut c = command("/bin/sleep 120 & printf child-started");
        c.process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = c.spawn().unwrap();
        let group = Pid::from_raw(child.id() as i32).unwrap();
        let mut guard = ChildGuard {
            child,
            group,
            reaped: false,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !exited(group).unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!darwin_zombie_only_group(group, guard.child.id()).unwrap());
        // Inject only the syscall errno. The process table and living child
        // are real, so the waiver cannot accidentally hide a live member.
        assert!(confirm_group_signal(&guard, Err(rustix::io::Errno::PERM)).is_err());
        stop_group(&guard).unwrap();
        guard.child.wait().unwrap();
        guard.reaped = true;
    }
    #[test]
    fn real_process_keeps_streams_separate_and_caps_infinite_output() {
        let runner = Runner {
            timeout: Duration::from_secs(2),
            output_limit: 16,
            cancelled: None,
        };
        let result = runner
            .run(&mut command("printf out; printf err >&2"), false)
            .unwrap();
        assert_eq!(result.stdout, "out");
        assert_eq!(result.stderr, "err");
        assert!(result.success);
        let start = Instant::now();
        let error = Runner {
            timeout: Duration::from_millis(100),
            output_limit: 64,
            cancelled: None,
        }
        .run(
            &mut command("while :; do printf '1234567890123456789012345678901234567890'; done"),
            false,
        )
        .err()
        .unwrap();
        assert_eq!(error, "Update command timed out.");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn held_descendant_pipe_and_cancel_do_not_defeat_deadline() {
        let start = Instant::now();
        let result = Runner {
            timeout: Duration::from_secs(2),
            output_limit: 64,
            cancelled: None,
        }
        .run(&mut command("/bin/sleep 120 & printf parent-exited"), false)
        .unwrap();
        assert_eq!(result.stdout, "parent-exited");
        assert!(start.elapsed() < Duration::from_secs(5));
        let flag = Arc::new(AtomicBool::new(false));
        let setter = flag.clone();
        let task = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            setter.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        let result = Runner {
            timeout: Duration::from_secs(120),
            output_limit: 64,
            cancelled: Some(&flag),
        }
        .run(&mut command("/bin/sleep 120"), false);
        task.join().unwrap();
        assert_eq!(result.err().unwrap(), "Update was cancelled.");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    #[ignore = "invoked by progress_does_not_block_on_an_unread_stdout_pipe"]
    fn unread_progress_pipe_helper() {
        let root = std::path::PathBuf::from(
            std::env::var_os("PDRS_UPDATE_PROGRESS_TEST").expect("parent-only helper"),
        );
        let signals = Signals::install().unwrap();
        let before = rustix::fs::fcntl_getfl(std::io::stdout()).unwrap();
        let result = if std::env::var_os("PDRS_UPDATE_WHOLE_OUTPUT").is_some() {
            struct Whole {
                root: std::path::PathBuf,
                cancelled: Arc<AtomicBool>,
            }
            impl super::super::Backend for Whole {
                fn check(&mut self) -> Result<super::super::Status, super::super::Error> {
                    Ok(super::super::Status {
                        reason: None,
                        can_update: true,
                        latest: Some(super::super::Release {
                            tag_name: "fixture".into(),
                            source_sha: "a".repeat(40),
                        }),
                    })
                }
                fn apply(
                    &mut self,
                    _target: &super::super::Release,
                    output: &mut dyn FnMut(&str),
                ) -> Result<(), super::super::Error> {
                    let mut c = command(
                        "printf '%s' \"$$\" > \"$1\"; i=0; while [ $i -lt 20000 ]; do printf '1234567890123456789012345678901234567890'; i=$((i+1)); done",
                    );
                    c.arg("finite-progress").arg(self.root.join("build-pid"));
                    Runner {
                        timeout: Duration::from_secs(15),
                        output_limit: 64,
                        cancelled: Some(&self.cancelled),
                    }
                    .run(&mut c, true)
                    .map_err(|e| super::super::Error::new("test", e))?;
                    // Runner is finished. This exact callback was the second
                    // blocking boundary when the surrounding guard was absent.
                    output("Build succeeded. Applying the prepared release.");
                    std::fs::write(
                        self.root.join("pid"),
                        std::fs::read(self.root.join("build-pid")).unwrap(),
                    )
                    .unwrap();
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !self.cancelled.load(Ordering::Relaxed) {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(super::super::Error::new(
                        "cancelled",
                        "Update was cancelled.",
                    ))
                }
                fn identity(&self) -> (&str, &str) {
                    ("fixture", "old")
                }
                fn target_generation(&self) -> Option<&str> {
                    None
                }
            }
            let mut backend = Whole {
                root: root.clone(),
                cancelled: signals.cancelled.clone(),
            };
            super::super::execute_stdout(
                super::super::Flags::default(),
                &mut backend,
                None,
                signals.cancelled.clone(),
            )
            .map(|_| ())
            .map_err(|e| e.message)
        } else {
            let mut c = command(
                "printf '%s' \"$$\" > \"$1\"; while :; do printf '1234567890123456789012345678901234567890' || exit; done",
            );
            c.arg("owned-progress-fixture").arg(root.join("pid"));
            Runner {
                timeout: Duration::from_secs(15),
                output_limit: 64,
                cancelled: Some(&signals.cancelled),
            }
            .run(&mut c, true)
            .map(|_| ())
        };
        let after = rustix::fs::fcntl_getfl(std::io::stdout()).unwrap();
        let pid = std::fs::read_to_string(root.join("pid"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let gone = rustix::process::test_kill_process_group(Pid::from_raw(pid).unwrap())
            == Err(rustix::io::Errno::SRCH);
        std::fs::write(root.join("result.tmp"),serde_json::to_vec(&serde_json::json!({"cancelled":result.err().as_deref()==Some("Update was cancelled."),"flagsRestored":before==after,"groupGone":gone})).unwrap()).unwrap();
        std::fs::rename(root.join("result.tmp"), root.join("result.json")).unwrap();
        // This dedicated child must not ask libtest to write its final line
        // after the parent deliberately closed the progress pipe.
        if std::env::var_os("PDRS_UPDATE_CLOSED_PIPE").is_some() {
            std::process::exit(0);
        }
    }
    #[test]
    fn progress_does_not_block_on_an_unread_stdout_pipe() {
        for (whole, close_pipe) in [(false, false), (true, false), (false, true)] {
            let root =
                std::env::temp_dir().join(format!("update-progress-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "update::process::tests::unread_progress_pipe_helper",
                    "--ignored",
                    "--nocapture",
                ])
                .env_clear()
                .env("PDRS_UPDATE_PROGRESS_TEST", &root)
                .env("HOME", &root)
                .env("TMPDIR", &root)
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if whole {
                command.env("PDRS_UPDATE_WHOLE_OUTPUT", "1");
            }
            if close_pipe {
                command.env("PDRS_UPDATE_CLOSED_PIPE", "1");
            }
            let mut child = command.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            while !root.join("pid").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !root.join("pid").exists() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("progress helper did not start");
            }
            // Deliberately do not drain stdout: fill the pipe before the real SIGINT.
            std::thread::sleep(Duration::from_millis(200));
            if close_pipe {
                drop(child.stdout.take());
            } else {
                rustix::process::kill_process(
                    Pid::from_raw(child.id() as i32).unwrap(),
                    Signal::INT,
                )
                .unwrap();
            }
            while !root.join("result.json").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !root.join("result.json").exists() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("update progress blocked cancellation on an unread stdout pipe");
            }
            let result: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("result.json")).unwrap()).unwrap();
            // The helper has now proved Runner returned and flags were restored.
            // Drain nonblocking only now, allowing the test harness's final line.
            let mut stdout = child.stdout.take();
            if let Some(stdout) = &stdout {
                let flags = rustix::fs::fcntl_getfl(stdout).unwrap();
                rustix::fs::fcntl_setfl(stdout, flags | rustix::fs::OFlags::NONBLOCK).unwrap();
            }
            let mut bytes = [0_u8; 8192];
            let status = loop {
                if let Some(stdout) = &mut stdout {
                    while matches!(stdout.read(&mut bytes),Ok(n)if n>0) {}
                }
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("progress helper did not exit after cancellation");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(status.success());
            assert_eq!(
                result,
                serde_json::json!({"cancelled":true,"flagsRestored":true,"groupGone":true})
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
