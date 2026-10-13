use std::path::Path;
use std::process::Command;

fn git_output(directory: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn watch_git_path(directory: &Path, name: &str) {
    if let Some(path) = git_output(
        directory,
        &["rev-parse", "--path-format=absolute", "--git-path", name],
    ) {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn main() {
    let directory =
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the package directory");
    let directory = Path::new(&directory);
    println!("cargo:rerun-if-changed=build.rs");
    // HEAD lives in the worktree git directory; refs and packed-refs may live
    // in the common git directory. Ask git for each path instead of assuming .git.
    watch_git_path(directory, "HEAD");
    watch_git_path(directory, "packed-refs");
    if let Some(reference) = git_output(directory, &["symbolic-ref", "-q", "HEAD"]) {
        watch_git_path(directory, &reference);
    }
    let commit = git_output(directory, &["rev-parse", "--verify", "HEAD"])
        .unwrap_or_else(|| "unregistered-build".to_owned());
    println!("cargo:rustc-env=PILOTDECK_SOURCE_COMMIT={commit}");
}
