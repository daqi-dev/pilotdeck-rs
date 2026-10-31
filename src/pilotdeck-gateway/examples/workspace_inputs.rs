//! Prebuild offline before running negative controls; execution never invokes Cargo.
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    env,
    path::{Path, PathBuf},
    process::Command,
};

#[allow(dead_code)]
#[path = "../tests/common/source_inputs.rs"]
mod source_inputs;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let result = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("#1166 git spawn");
    assert!(
        result.status.success(),
        "#1166 git failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn main() {
    let args: Vec<_> = env::args().collect();
    assert_eq!(args.len(), 3, "#1166 usage: workspace_inputs ROOT BASELINE");
    let root = Path::new(&args[1])
        .canonicalize()
        .expect("#1166 repository root");
    let head = String::from_utf8(git(
        &root,
        &["rev-parse", "--verify", &format!("{}^{{commit}}", args[2])],
    ))
    .expect("#1166 git OID")
    .trim()
    .to_owned();
    let inventory = git(
        &root,
        &["ls-tree", "-r", "--name-only", "-z", &head, "--", "src"],
    );
    let mut pending = Vec::new();
    let mut inputs = BTreeSet::new();
    for name in inventory
        .split(|byte| *byte == 0)
        .filter(|name| name.ends_with(b".rs"))
    {
        let relative = std::str::from_utf8(name).expect("#1166 UTF-8 source path");
        let path = source_inputs::workspace_target(&root, &root.join(relative));
        inputs.insert(relative.to_owned());
        pending.push((path.clone(), source_inputs::source_module_dir(&path)));
    }
    let mut seen = HashSet::<(PathBuf, PathBuf)>::new();
    while let Some((path, module_dir)) = pending.pop() {
        if !seen.insert((path.clone(), module_dir.clone())) {
            continue;
        }
        for edge in source_inputs::workspace_source_edges(&root, &path, module_dir) {
            inputs.insert(
                edge.path
                    .strip_prefix(&root)
                    .expect("#1166 repository input")
                    .to_str()
                    .expect("#1166 UTF-8 input path")
                    .to_owned(),
            );
            if edge.rust_source {
                pending.push((edge.path, edge.module_dir));
            }
        }
    }
    // The caller checks the embedded parser identity against the installed source,
    // so a stale prebuilt binary cannot silently accept a changed guard.
    let mut hash = Sha256::new();
    hash.update(include_str!("workspace_inputs.rs"));
    hash.update(include_str!("../tests/common/source_inputs.rs"));
    hash.update(include_str!("../Cargo.toml"));
    hash.update(include_str!("../../Cargo.toml"));
    hash.update(include_str!("../../Cargo.lock"));
    println!(
        "{}",
        serde_json::json!({"head": head, "root": root, "scanner_sha256": format!("{:x}", hash.finalize()), "inputs": inputs})
    );
}
