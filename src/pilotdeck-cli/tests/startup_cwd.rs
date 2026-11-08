//! The process starts in a directory removed after chdir but before exec.
#[cfg(unix)]
#[test]
fn deleted_startup_directory_reports_filesystem_error() {
    use std::{ffi::CString, os::unix::{ffi::OsStrExt, process::CommandExt}, process::Command};
    let root = std::env::temp_dir().join(format!("pdrs-startup-cwd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = CString::new(root.as_os_str().as_bytes()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck"));
    child.current_dir(&root).env_clear().env("HOME", &root).env("PILOT_HOME", &root);
    // rmdir is async-signal-safe; the closure uses only the captured C string.
    unsafe { child.pre_exec(move || {
        if libc::rmdir(path.as_ptr()) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    }); }
    let result = child.output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert_eq!(result.stderr, b"ENOENT: no such file or directory, uv_cwd\n");
    assert!(!root.exists());
}
