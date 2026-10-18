//! Subprocess checks keep all default/custom paths within a disposable home.
use std::{fs, process::Command};

#[test]
fn usage_json_error_exit_and_gateway_todo_are_distinct() {
    let root = std::env::temp_dir().join(format!("pdrs-skills-exit-{}", std::process::id()));
    fs::create_dir_all(root.join("source/bad")).unwrap();
    fs::write(root.join("source/bad/SKILL.md"), "no frontmatter").unwrap();
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
            .env_clear()
            .env("HOME", root.join("home"))
            .env("PILOT_HOME", root.join("pilot"))
            .env("LC_ALL", "C")
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap()
    };
    let usage = invoke(&["skills", "unknown"]);
    assert_eq!(usage.status.code(), Some(1));
    assert!(usage.stdout.is_empty());
    assert_eq!(
        String::from_utf8(usage.stderr).unwrap(),
        "Usage: pilotdeck skills migrate [--execute] [--from cc,openclaw,hermes] [--source <dir>] [--overwrite|--rename]\n"
    );
    let text = invoke(&["skills", "migrate", "--source", "source"]);
    let json = invoke(&["skills", "migrate", "--source", "source", "--json"]);
    assert_eq!(text.status.code(), Some(1));
    assert_eq!(json.status.code(), Some(0));
    assert!(text.stderr.is_empty() && json.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["summary"]["error"], 1);
    let todo = invoke(&["server"]);
    assert_eq!(todo.status.code(), Some(2));
    assert!(
        String::from_utf8(todo.stderr)
            .unwrap()
            .starts_with("pilotdeck-rs TODO: ")
    );
    assert!(!root.join("pilot").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_success_notice_is_stdout_before_config_error() {
    let root = std::env::temp_dir().join(format!("pdrs-legacy-streams-{}", std::process::id()));
    let home = root.join("pilot");
    let builtin = root.join("bundled");
    for dir in [home.join("skills/sample"), builtin.join("sample")] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "same").unwrap();
    }
    fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pilotdeck"))
        .env_clear()
        .env("HOME", root.join("home"))
        .env("PILOT_HOME", &home)
        .env("PILOTDECK_BUNDLED_SKILLS_DIR", &builtin)
        .env("PILOTDECK_CONFIG_PATH", home.join("pilotdeck.yaml"))
        .env("LC_ALL", "C")
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(
            "[pilotdeck] Activated bundled skills directly; moved 1 unchanged legacy copy to {}.\n",
            home.join("skill-backups/legacy-bundled-v1").display()
        )
    );
    assert_eq!(out.stderr, b"Config must contain an agent section.\n");
    assert!(!home.join("skills/sample").exists());
    assert!(
        home.join("skill-backups/legacy-bundled-v1/sample/SKILL.md")
            .exists()
    );
    fs::remove_dir_all(root).unwrap();
}
