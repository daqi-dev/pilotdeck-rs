use std::{fs, path::Path};

fn section<'a>(manifest: &'a str, name: &str) -> Vec<&'a str> {
    manifest
        .lines()
        .map(|line| line.split('#').next().unwrap().trim())
        .skip_while(|line| *line != name)
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.is_empty())
        .collect()
}

fn entries(path: &Path) -> Vec<String> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    entries.sort();
    entries
}

#[test]
fn compiler_enforces_gateway_boundary() {
    let gateway = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = gateway.parent().unwrap();
    let manifest = fs::read_to_string(gateway.join("Cargo.toml")).unwrap();
    let workspace_manifest = fs::read_to_string(workspace.join("Cargo.toml")).unwrap();
    assert_eq!(section(&manifest, "[lints]"), ["workspace = true"]);
    assert_eq!(
        section(&workspace_manifest, "[workspace.lints.rust]"),
        ["unsafe_code = \"forbid\""]
    );

    // Only this small crate may override the workspace's Rust lint level.
    let child = workspace.join("pilotdeck-child-signals");
    assert_eq!(entries(&child), ["Cargo.toml", "src"]);
    assert_eq!(entries(&child.join("src")), ["lib.rs"]);
    for (relative, is_dir) in [("Cargo.toml", false), ("src", true), ("src/lib.rs", false)] {
        let kind = fs::symlink_metadata(child.join(relative))
            .unwrap()
            .file_type();
        assert!(!kind.is_symlink(), "{relative} must not be a symlink");
        assert_eq!(kind.is_dir(), is_dir, "{relative}");
        assert_eq!(kind.is_file(), !is_dir, "{relative}");
    }
    let manifest = fs::read_to_string(child.join("Cargo.toml")).unwrap();
    assert_eq!(
        section(&manifest, "[lints.rust]"),
        ["unsafe_code = \"deny\""]
    );
    assert_eq!(
        section(&manifest, "[lints.clippy]"),
        section(&workspace_manifest, "[workspace.lints.clippy]")
    );
    assert_eq!(
        section(&manifest, "[dependencies]"),
        ["libc = \"=0.2.190\""]
    );
    let source = fs::read_to_string(child.join("src/lib.rs")).unwrap();
    assert_eq!(source.matches("#[allow(unsafe_code)]").count(), 1);
    assert_eq!(source.matches("allow(unsafe_code)").count(), 1);
    assert!(source.contains("#![cfg(unix)]"));
    for forbidden in ["macro_rules", "include", "#[path", "mod "] {
        assert!(
            !source.contains(forbidden),
            "child crate contains {forbidden}"
        );
    }
}
