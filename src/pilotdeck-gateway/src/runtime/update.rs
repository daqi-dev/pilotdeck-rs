//! CLI and IM update operation (`src/runtime/updateCommand.ts:6-26`).
//!
//! The updater is a deployment asset owned by the Web release service. This
//! module owns discovery, execution, streaming and the bounded command result.
use pilotdeck_protocol::JsString;
use crate::metadata::js_space;
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
};

const MAX_TAIL: usize = 12_000;
const MISSING_UPDATER: &str = "This installation does not include the Git Web updater. Use application settings or update manually.";

// Source-only tests need a deployment anchor without changing process-wide
// environment or installing fixture files into the checkout.
#[cfg(test)]
thread_local! {
    static TEST_DEPLOYMENT_ANCHOR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_deployment_anchor_for_tests<T>(start: &Path, run: impl FnOnce() -> T) -> T {
    struct Reset(Option<PathBuf>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_DEPLOYMENT_ANCHOR.with(|anchor| anchor.replace(self.0.take()));
        }
    }
    let _reset = Reset(TEST_DEPLOYMENT_ANCHOR.with(|anchor| anchor.replace(Some(start.into()))));
    run()
}

#[derive(Debug, PartialEq, Eq)]
pub struct UpdateResult {
    /// Raw child exit code. CLI treats both 0 and 2 as success; IM distinguishes 2.
    pub code: i32,
    /// JS strings are UTF-16: slicing the tail can retain a lone surrogate.
    pub output: JsString,
}

/// `updateCommand.ts:6-10` — compiled module directory, not the crate root.
pub fn update_module_start() -> PathBuf {
    #[cfg(test)]
    if let Some(start) = TEST_DEPLOYMENT_ANCHOR.with(|anchor| anchor.borrow().clone()) {
        return start;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime")
}

/// `start` is a directory, as in TS findUpdateRoot. No cwd fallback is used.
pub fn find_update_root(start: &Path) -> io::Result<PathBuf> {
    for current in start.ancestors() {
        if current.join("scripts/update-web.mjs").exists() {
            return Ok(current.to_path_buf());
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, MISSING_UPDATER))
}

pub fn find_update_root_from_deployment() -> io::Result<PathBuf> {
    #[cfg(test)]
    if let Some(start) = TEST_DEPLOYMENT_ANCHOR.with(|anchor| anchor.borrow().clone()) {
        return find_update_root(&start);
    }
    let executable = std::env::current_exe()?;
    find_update_root(executable.parent().unwrap_or(&executable))
        .or_else(|_| find_update_root(&update_module_start()))
}

/// Git checkout root, or the package that ships `scripts/update-web.mjs`.
pub fn find_installation_root(start: &Path) -> Option<PathBuf> {
    super::update_release::find_git_root(start)
        .or_else(|| find_update_root(start).ok())
        .or_else(|| find_update_root(&update_module_start()).ok())
}

pub fn web_update_command_root() -> Option<PathBuf> {
    for start in [update_module_start()].into_iter().chain(
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .into_iter(),
    ) {
        for current in start.ancestors() {
            if current.join("ui/server/services").is_dir() {
                return Some(current.to_path_buf());
            }
        }
    }
    None
}

/// `webUpdateCommand.js:7` installation root when git/update-web discovery fails.
pub fn default_web_update_project_root() -> Option<PathBuf> {
    find_update_root_from_deployment()
        .ok()
        .or_else(web_update_command_root)
}

/// Shared production entry. The injected variant also supports packaged Node
/// paths and deterministic process tests without executing a real updater.
pub fn run_update_process(
    args: &[String],
    output: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<UpdateResult> {
    let mut sink = output;
    match find_update_root_from_deployment() {
        Ok(root)
            if root
                .join("ui/server/services/webUpdateCommand.js")
                .is_file() =>
        {
            run_update_process_from(
                &root,
                &crate::host::deployment::update_js_executable(),
                args,
                sink,
            )
        }
        Ok(_) if super::update_release::find_git_root(&update_module_start()).is_some() => {
            Err(io::Error::other(
                "development: Developer branches/worktrees and development runtimes do not support self-update.",
            ))
        }
        _ => run_native_update_process(args, &mut sink),
    }
}

fn run_native_update_process(
    args: &[String],
    output: &mut dyn FnMut(&str) -> io::Result<()>,
) -> io::Result<UpdateResult> {
    if args
        .iter()
        .any(|arg| !matches!(arg.as_str(), "--check" | "--restart"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Usage: update [--check] [--restart]",
        ));
    }
    let mut backend =
        super::update_release::NativeUpdateBackend::from_current_exe().map_err(io::Error::other)?;
    let mut tail = Vec::new();
    let mut sink_error = None;
    let code = super::update_service::run_web_update_command(
        &mut backend,
        args.iter().any(|arg| arg == "--check"),
        args.iter().any(|arg| arg == "--restart"),
        &mut |line| {
            let chunk = format!("{line}\n");
            tail.extend(chunk.encode_utf16());
            if tail.len() > MAX_TAIL {
                tail.drain(..tail.len() - MAX_TAIL);
            }
            if sink_error.is_none() {
                sink_error = output(&chunk).err();
            }
        },
    )
    .map_err(io::Error::other)?;
    if let Some(error) = sink_error {
        return Err(error);
    }
    let space = |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(js_space);
    let start = tail
        .iter()
        .position(|unit| !space(unit))
        .unwrap_or(tail.len());
    let end = tail
        .iter()
        .rposition(|unit| !space(unit))
        .map_or(start, |at| at + 1);
    Ok(UpdateResult {
        code,
        output: JsString::from_utf16(tail[start..end].to_vec()),
    })
}

pub fn run_update_process_from(
    project_root: &Path,
    executable: &Path,
    args: &[String],
    mut output: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<UpdateResult> {
    let mut command = Command::new(executable);
    command
        .arg(project_root.join("scripts/update-web.mjs"))
        .args(args)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // windowsHide / CREATE_NO_WINDOW
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (sender, chunks) = mpsc::channel();
    let out_sender = sender.clone();
    let stdout_thread = thread::spawn(move || collect(stdout, out_sender));
    let stderr_thread = thread::spawn(move || collect(stderr, sender));
    let mut tail = Vec::new();
    let mut error = None;
    for chunk in chunks {
        match chunk {
            Ok(chunk) => {
                // Buffer.toString() decodes each data chunk separately in TS.
                let text = String::from_utf8_lossy(&chunk);
                tail.extend(text.encode_utf16());
                if tail.len() > MAX_TAIL {
                    tail.drain(..tail.len() - MAX_TAIL);
                }
                if error.is_none() {
                    error = output(&text).err();
                    if error.is_some() {
                        // Still drain and reap on a broken output sink.
                        let _ = child.kill();
                    }
                }
            }
            Err(failure) => {
                if error.is_none() {
                    error = Some(failure);
                    let _ = child.kill();
                }
            }
        }
    }
    let status = child.wait();
    let stdout_result = stdout_thread.join();
    let stderr_result = stderr_thread.join();
    if let Some(error) = error {
        return Err(error);
    }
    for result in [stdout_result, stderr_result] {
        result.map_err(|_| io::Error::other("update output reader stopped unexpectedly"))??;
    }
    let status = status?;
    let space = |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(js_space);
    let start = tail
        .iter()
        .position(|unit| !space(unit))
        .unwrap_or(tail.len());
    let end = tail
        .iter()
        .rposition(|unit| !space(unit))
        .map_or(start, |at| at + 1);
    Ok(UpdateResult {
        code: status.code().unwrap_or(1),
        output: JsString::from_utf16(tail[start..end].to_vec()),
    })
}

fn collect(mut input: impl Read, output: mpsc::Sender<io::Result<Vec<u8>>>) -> io::Result<()> {
    let mut buffer = [0; 4096];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                if output.send(Ok(buffer[..read].to_vec())).is_err() {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = output.send(Err(io::Error::new(error.kind(), error.to_string())));
                return Err(error);
            }
        }
    }
}

/// IM `/update` production entry (`ChannelCommandRegistry.ts:135-157`); delivery via
/// `channel_commands::execute_channel_command` and channel owners such as Weixin.
pub fn run_channel_update(arg: &str, reply: impl FnMut(&str)) {
    run_channel_update_with(arg, reply, |args| run_update_process(args, |_| Ok(())));
}

pub fn run_channel_update_with(
    arg: &str,
    mut reply: impl FnMut(&str),
    run: impl FnOnce(&[String]) -> io::Result<UpdateResult>,
) {
    let arg = crate::metadata::js_trim(arg);
    if !arg.is_empty() && arg != "check" {
        reply(
            "用法：/update check 检查 Release；/update 更新并请求重启。未运行 Web 服务时，请使用 CLI 手动更新。",
        );
        return;
    }
    reply(if arg == "check" {
        "⏳ 正在检查 Release 更新..."
    } else {
        "⏳ 正在检查部署资格并更新 Release，完成后向 Web 服务请求重启..."
    });
    match run(&[if arg == "check" {
        "--check"
    } else {
        "--restart"
    }
    .into()])
    {
        Ok(result) if result.code == 2 => reply("✅ 已是最新 Release，无需更新。"),
        Ok(result) => {
            let output = String::from_utf16_lossy(result.output.units());
            let message = output
                .split('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line))
                .rfind(|line| !line.is_empty())
                .unwrap_or("更新命令未返回结果，请查看设置页。");
            reply(&format!(
                "{} {message}",
                if result.code == 0 { "✅" } else { "❌" }
            ));
        }
        Err(error) => reply(&format!("❌ 更新失败：{error}")),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn fixture(name: &str, script: &str) -> (crate::scratch::Scratch, PathBuf) {
        let scratch = crate::scratch::Scratch::new(name);
        fs::create_dir_all(scratch.path().join("scripts")).unwrap();
        fs::write(scratch.path().join("scripts/update-web.mjs"), "fixture").unwrap();
        let executable = scratch.path().join("mock-node");
        fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        (scratch, executable)
    }

    #[test]
    fn directory_discovery_and_missing_installation_match_ts() {
        let (scratch, _) = fixture("update-discovery", "exit 0");
        let nested = scratch.path().join("nested/deep");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_update_root(&nested).unwrap(), scratch.path());
        assert_eq!(
            find_installation_root(scratch.path().join("nested/deep").as_path()),
            Some(scratch.path().to_path_buf())
        );
        assert_eq!(
            find_update_root(Path::new("/")).unwrap_err().to_string(),
            MISSING_UPDATER
        );
    }

    #[test]
    fn raw_exit_codes_arguments_cwd_and_both_streams_are_preserved() {
        for code in [0, 2, 7] {
            let (scratch, executable) = fixture(
                &format!("update-code-{code}"),
                &format!(
                    "test \"$PWD/scripts/update-web.mjs\" -ef \"$1\" || exit 9\nprintf '%s\\n' \"$2\"\nprintf 'stderr\\n' >&2\nexit {code}"
                ),
            );
            let mut streamed = String::new();
            let result = run_update_process_from(
                scratch.path(),
                &executable,
                &["--check".into()],
                |chunk| {
                    streamed.push_str(chunk);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(result.code, code);
            assert!(streamed.contains("--check\n") && streamed.contains("stderr\n"));
            assert_eq!(result.output.to_utf8().unwrap(), streamed.trim());
        }
    }

    #[test]
    fn tail_uses_utf16_units_and_ecmascript_trim() {
        let (scratch, executable) = fixture("update-tail", "exec cat \"$PWD/output\"");
        fs::write(
            scratch.path().join("output"),
            format!("prefix{}\u{feff}", "中😀".repeat(6000)),
        )
        .unwrap();
        let result = run_update_process_from(scratch.path(), &executable, &[], |_| Ok(())).unwrap();
        assert_eq!(result.output.units().len(), 11999);
        assert!(
            result
                .output
                .units()
                .ends_with(&"中😀".encode_utf16().collect::<Vec<_>>())
        );
        assert!(!String::from_utf16_lossy(result.output.units()).contains("prefix"));
    }

    #[test]
    fn spawn_and_output_errors_are_returned() {
        let (scratch, executable) = fixture("update-errors", "printf 'output'\nexit 0");
        assert!(
            run_update_process_from(scratch.path(), &scratch.path().join("absent"), &[], |_| Ok(
                ()
            ))
            .is_err()
        );
        let error = run_update_process_from(scratch.path(), &executable, &[], |_| {
            Err(io::Error::other("broken sink"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "broken sink");
    }

    #[test]
    fn find_update_root_from_deployment_checks_module_anchor() {
        let scratch = crate::scratch::Scratch::new("update-module-anchor");
        let root = scratch.path();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("scripts/update-web.mjs"), "fixture").unwrap();
        let anchor = root.join("gateway/src/runtime");
        fs::create_dir_all(&anchor).unwrap();
        assert_eq!(find_update_root(&anchor).unwrap(), root);
    }

    #[test]
    fn update_module_anchor_finds_shipped_script_in_repo() {
        let scratch = crate::scratch::Scratch::new("update-shipped-script");
        let anchor = scratch.path().join("gateway/src/runtime");
        fs::create_dir_all(&anchor).unwrap();
        fs::create_dir_all(scratch.path().join("scripts")).unwrap();
        fs::write(
            scratch.path().join("scripts/update-web.mjs"),
            "process.exit(0);\n",
        )
        .unwrap();
        with_deployment_anchor_for_tests(&anchor, || {
            let root = find_update_root(&update_module_start()).unwrap();
            assert_eq!(root, scratch.path());
            assert_eq!(find_update_root_from_deployment().unwrap(), root);
            assert!(
                root.join("scripts/update-web.mjs").is_file(),
                "production anchor must discover scripts/update-web.mjs"
            );
        });
    }

    #[test]
    fn im_handler_uses_shared_operation_and_only_relays_the_outcome() {
        for (arg, code, expected_arg, last) in [
            ("check", 2, "--check", "✅ 已是最新 Release，无需更新。"),
            ("", 0, "--restart", "✅ prepared"),
            ("check", 1, "--check", "❌ prepared"),
        ] {
            let mut replies = Vec::new();
            run_channel_update_with(
                arg,
                |message| replies.push(message.to_owned()),
                |args| {
                    assert_eq!(args, &[expected_arg]);
                    Ok(UpdateResult {
                        code,
                        output: "private build log\r\nprepared".into(),
                    })
                },
            );
            assert_eq!(replies.len(), 2);
            assert_eq!(replies.last().unwrap(), last);
        }
        let mut replies = Vec::new();
        run_channel_update_with(
            "other",
            |message| replies.push(message.to_owned()),
            |_| panic!("invalid argument invoked updater"),
        );
        assert_eq!(replies.len(), 1);
        run_channel_update_with(
            "check",
            |message| replies.push(message.to_owned()),
            |_| Err(io::Error::other("spawn failed")),
        );
        assert_eq!(replies.last().unwrap(), "❌ 更新失败：spawn failed");
    }
}
