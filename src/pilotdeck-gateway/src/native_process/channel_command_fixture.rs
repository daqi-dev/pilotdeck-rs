//! Shared actual-channel fixture boundary: real git/status runner and a mock installed CLI.
//! No release lookup, real update, service restart, global git config, or external account.
use std::path::PathBuf;
pub(super) fn prepare(options: &super::Options) -> PathBuf {
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .args(args)
            .current_dir(&options.project)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &options.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "isolated git fixture failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(&["-c", "init.defaultBranch=fixture", "init", "--quiet"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "mock channel status",
    ]);
    let cli = options.home.join("mock-channel-pilotdeck");
    std::fs::write(
        &cli,
        r#"#!/bin/sh
auth=no
if [ -n "${PILOTDECK_GATEWAY_TOKEN:-}" ]; then auth=yes; fi
printf '%s|auth=%s\n' "$*" "$auth" >> "$PILOT_HOME/channel-update-calls.log"
case "$*" in
  "update --check") printf 'mock-release-current\n'; exit 2 ;;
  "update --restart") [ "$auth" = yes ] || exit 9; printf 'mock-release-restart\n'; exit 0 ;;
  *) printf 'unexpected mock CLI args\n' >&2; exit 8 ;;
esac
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}
pub(super) fn project_name(text: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix("• "))
        .expect("real project catalog must expose fixture project")
        .trim_end_matches(" ✅")
        .to_owned()
}
pub(super) fn assert_update_calls(options: &super::Options) {
    let calls = std::fs::read_to_string(options.home.join("channel-update-calls.log")).unwrap();
    assert!(
        calls
            .lines()
            .any(|line| line.starts_with("update --check|")),
        "real check CLI invocation missing: {calls}"
    );
    assert!(
        calls
            .lines()
            .any(|line| line == "update --restart|auth=yes"),
        "real restart CLI invocation must receive mock Gateway auth: {calls}"
    );
    assert_eq!(calls.lines().count(), 2, "no unexpected CLI invocations");
}

pub(super) async fn control(
    send: impl FnOnce(&str),
    replies: impl Fn() -> Vec<String>,
    model_count: impl Fn() -> usize,
    input: &str,
    expected: &str,
) -> String {
    let start = replies().len();
    let before = model_count();
    send(input);
    let answer = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if let Some(text) = replies()
                .get(start..)
                .unwrap_or(&[])
                .iter()
                .find(|text| text.contains(expected))
                .cloned()
            {
                break text;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "actual command {input} expected {expected}; replies={:#?}",
            replies()
        )
    });
    assert_eq!(
        model_count(),
        before,
        "system command {input} must bypass actual A7"
    );
    answer
}
