//! The process starts in a directory removed after chdir but before exec.
#[cfg(unix)]
#[test]
fn deleted_startup_directory_reports_filesystem_error() {
    use std::process::Command;
    let root = std::env::temp_dir().join(format!("pdrs-startup-cwd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    // The shell starts while cwd exists, then removes only this empty scratch
    // directory and execs the CLI. No unsafe post-fork Rust closure is needed.
    let result = Command::new("/bin/sh")
        .args(["-c", "/bin/rmdir -- \"$1\" && exec \"$2\"", "startup-cwd"])
        .arg(&root)
        .arg(env!("CARGO_BIN_EXE_pilotdeck"))
        .current_dir(&root)
        .env_clear()
        .env("HOME", &root)
        .env("PILOT_HOME", &root)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert_eq!(
        result.stderr,
        b"ENOENT: no such file or directory, uv_cwd\n"
    );
    assert!(!root.exists());
}
