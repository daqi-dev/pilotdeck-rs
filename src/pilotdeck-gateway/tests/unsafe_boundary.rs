//! 守卫边界：检查 workspace 成员源码的 lint 继承与 manifest、child crate
//! 内容、全部 workspace 成员的解析后依赖闭包及其所有声明依赖的 source/path
//! （含 dev/build/target 与未启用的可选依赖）、仓内 Cargo 配置与 toolchain 文件、
//! workspace manifest 树内的测试 cwd 到根的 Cargo 配置、CARGO_HOME（未设时 ~/.cargo）的
//! config*，以及测试进程可见的编译相关环境变量。
//! CI yml 与脚本自行替换编译器（包括 source .env、设置 CARGO_HOME、RUSTC /
//! CARGO 包装器）由人工评审负责，不靠扫描脚本文本覆盖；依据：
//! state/decisions-20261021.md「卡 · #386 守卫的构建配置检查改为「一律不许存在」，并划清守卫边界（AI 代批，待人复核）」。
//! source 只接受 null（已审计本地包）与 registry+；git 等其它 kind 一律拒绝。
//! vendor 与 registry 依赖内部的 unsafe 不属于本守卫的审计范围；registry
//! 依赖由 Cargo.lock pin 评审，既有 vendor pin 的变更仍须按冻结件规则评审。

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use sha2::{Digest, Sha256};

fn stripped_lines(manifest: &str) -> Vec<&str> {
    manifest
        .lines()
        .map(|line| line.split('#').next().unwrap().trim())
        .collect()
}

fn section<'a>(manifest: &'a str, name: &str) -> Vec<&'a str> {
    stripped_lines(manifest)
        .into_iter()
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

// This guard deliberately accepts only explicit, quoted workspace paths. A new
// manifest shape must extend the guard rather than silently skip a member.
fn workspace_paths(manifest: &str, key: &str) -> Vec<String> {
    let workspace = section(manifest, "[workspace]");
    let prefix = format!("{key} = [");
    let start = workspace
        .iter()
        .position(|line| line.starts_with(&prefix))
        .expect("explicit workspace path list");
    let members = workspace[start..].join("\n");
    let members = members.strip_prefix(&prefix).unwrap();
    let (members, _) = members.split_once(']').expect("closed members list");
    let members: Vec<_> = members
        .split(',')
        .map(str::trim)
        .filter(|member| !member.is_empty())
        .map(|member| {
            let member = member
                .strip_prefix('"')
                .and_then(|member| member.strip_suffix('"'))
                .expect("quoted workspace member");
            assert!(
                member
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "-_/".contains(ch)),
                "unsupported workspace member: {member}"
            );
            assert!(!member.is_empty() && !member.starts_with('/'));
            member.to_owned()
        })
        .collect();
    assert!(!members.is_empty());
    members
}

const VENDORED_PACKAGES: [&str; 2] = ["vendor/tiktoken-rs", "data/icu_collator_data"];

fn is_local_source(source: &Value, description: &str) -> bool {
    match source {
        Value::Null => true,
        Value::String(source) if source.starts_with("registry+") => false,
        _ => panic!("unsupported dependency source for {description}: {source}"),
    }
}

fn resolved_dependency_boundary(workspace: &Path, members: &[String]) {
    let output = Command::new(std::env::var_os("CARGO").expect("Cargo executable from CARGO"))
        .args(["metadata", "--format-version", "1"])
        .current_dir(workspace)
        .output()
        .expect("run cargo metadata (honoring Cargo network policy)");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).expect("Cargo metadata JSON");
    let packages: HashMap<_, _> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|package| (package["id"].as_str().unwrap(), package))
        .collect();
    let package_directory = |package: &Value| {
        Path::new(package["manifest_path"].as_str().unwrap())
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
    };
    let member_ids: HashSet<_> = metadata["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    let resolved_members: BTreeSet<_> = member_ids
        .iter()
        .map(|id| package_directory(packages[id]))
        .collect();
    let checked_members: BTreeSet<_> = members
        .iter()
        .map(|member| workspace.join(member).canonicalize().unwrap())
        .collect();
    assert_eq!(checked_members.len(), members.len(), "duplicate members");
    assert_eq!(
        resolved_members, checked_members,
        "Cargo workspace members must match the lint-checked manifest members"
    );

    let vendored: BTreeSet<_> = VENDORED_PACKAGES
        .iter()
        .map(|relative| workspace.join(relative).canonicalize().unwrap())
        .collect();
    let nodes: HashMap<_, _> = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolved dependency graph")
        .iter()
        .map(|node| (node["id"].as_str().unwrap(), node))
        .collect();
    let mut pending: Vec<_> = member_ids.iter().copied().collect();
    let mut visited = HashSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let package = packages[id];
        if is_local_source(package.get("source").expect("package source"), id) {
            let directory = package_directory(package);
            assert!(
                (member_ids.contains(id) && checked_members.contains(&directory))
                    || vendored.contains(&directory),
                "unknown local package in workspace resolved dependency closure: {id} ({})",
                directory.display()
            );
        }
        if member_ids.contains(id) {
            // Package declarations include optional and target-specific edges
            // even when they are absent from the active resolve graph. Cargo
            // normalizes inline/table/workspace-inherited dependency syntax.
            for dependency in package["dependencies"].as_array().unwrap() {
                let name = dependency["name"].as_str().unwrap();
                if is_local_source(
                    dependency
                        .get("source")
                        .expect("declared dependency source"),
                    &format!("{id} dependency {name}"),
                ) {
                    let directory = Path::new(
                        dependency
                            .get("path")
                            .and_then(Value::as_str)
                            .expect("local dependency path"),
                    )
                    .canonicalize()
                    .expect("existing dependency directory");
                    assert!(
                        checked_members.contains(&directory) || vendored.contains(&directory),
                        "unknown declared path dependency of workspace member {id}: {} ({})",
                        dependency["name"].as_str().unwrap(),
                        directory.display()
                    );
                }
            }
        }
        // Cargo's dependencies list includes every resolved edge, without
        // filtering dep_kinds: normal, build and dev dependencies all count.
        pending.extend(
            nodes[id]["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .map(|dependency| dependency.as_str().unwrap()),
        );
    }
}

// Skip nested comments and ordinary/raw strings before counting Rust tokens.
// Character literals are skipped too; lifetimes remain ordinary tokens.
fn rust_tokens(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut tokens = Vec::new();
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while depth != 0 {
                assert!(i < bytes.len(), "unclosed comment");
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'r'
            && bytes[i + 1..].iter().copied().find(|&b| b != b'#') == Some(b'"')
        {
            let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
            i += hashes + 2;
            loop {
                assert!(i < bytes.len(), "unclosed raw string");
                if bytes[i] == b'"'
                    && bytes
                        .get(i + 1..i + 1 + hashes)
                        .is_some_and(|suffix| suffix.iter().all(|&b| b == b'#'))
                {
                    i += hashes + 1;
                    break;
                }
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i += 1;
            loop {
                assert!(i < bytes.len(), "unclosed string");
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else if bytes[i] == b'\''
            && (bytes.get(i + 2) == Some(&b'\'')
                || (bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'')))
        {
            i += if bytes[i + 1] == b'\\' { 4 } else { 3 };
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(source[start..i].to_owned());
        } else {
            if !bytes[i].is_ascii_whitespace() {
                tokens.push(char::from(bytes[i]).to_string());
            }
            i += 1;
        }
    }
    tokens
}

// The complete expected content of pilotdeck-child-signals/src/lib.rs
// (comments stripped by `rust_tokens`, so comment-only edits pass; every
// other change — imports, type aliases shadowing `libc`, helper items, a
// moved or copied unsafe block — changes the token sequence). ANY change to
// that file must update this copy in the same commit and re-extends the
// #386/#552/#621 human-approved unsafe exception; do not edit either side alone.
// Source order: the #621 SIGXFSZ ignore, then the #386 signal reset, then the
// #552 setsid.
const CHILD_SOURCE: &str = r#"
//! The #386 human-approved exception: reset inherited child signals before exec.
//! The #552 human-approved addition: libuv's `detached` `setsid()` in the same
//! fork-to-exec window, for every spawn whose TS counterpart is detached (the
//! bash runner, background tasks, the ripgrep engine spawn and command hooks;
//! A6.5-BASH-OS-001 setsid half).
//! The #621 fix-r4 addition: this process's own SIGXFSZ disposition, matching
//! Node's process-wide ignore (see [`ignore_sigxfsz`]).
#![cfg(unix)]

use std::{io, os::unix::process::CommandExt, process::Command};

/// Install `SIG_IGN` for SIGXFSZ in this process, aligning it with Node.
/// Node's platform init ignores SIGXFSZ, so a write past RLIMIT_FSIZE
/// surfaces as an `EFBIG` error from the syscall; Rust std ignores only
/// SIGPIPE, so the same write instead kills the process (probe on node
/// v22.22.0 vs rustc 1.99.0, `ulimit -f 1` writing 4 KiB: Node gets
/// `{"errno":-27,"code":"EFBIG","syscall":"write"}` and continues, the Rust
/// process dies with exit 153 before user code sees any io::Error; evidence
/// `_tmp/20261025-issue-621-takeover/xfsz-probe/`). The gateway binary calls
/// this at startup, and the debug request dump installs it before its write
/// (`runtime/model/http.rs`), so every host process of the model runtime is
/// covered — that runtime has no process entry of its own. Idempotent and
/// thread-safe (concurrent identical installs are benign); an error means
/// the disposition was left unchanged, never made worse.
#[allow(unsafe_code)]
pub fn ignore_sigxfsz() -> io::Result<()> {
    // SAFETY: sigaction(2) only changes this process's disposition for
    // SIGXFSZ. All pointers refer to live stack storage; the null old-action
    // pointer requests no output. The zeroed struct is valid on macOS/Linux
    // (integer fields and a nullable sa_restorer); its mask is initialized
    // before use and its handler is SIG_IGN, which the kernel never invokes.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_IGN;
        if libc::sigemptyset(&mut action.sa_mask) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::sigaction(libc::SIGXFSZ, &action, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

// libuv's Unix child-init range is 1..32, excluding unchangeable signals.
// This is 29 signals on macOS and Linux; Linux 32/33 (glibc's internal
// realtime signals), and all other realtime dispositions, are left alone.
// Build the list at compile time, never by querying libc after fork.
const CHILD_SIGNALS: [libc::c_int; 29] = {
    let mut signals = [0; 29];
    let mut signal = 1;
    let mut index = 0;
    while signal < 32 {
        if signal != libc::SIGKILL && signal != libc::SIGSTOP {
            signals[index] = signal;
            index += 1;
        }
        signal += 1;
    }
    signals
};

/// Install before spawn, after any caller-owned pre-exec hooks. Tokio callers
/// pass `as_std_mut()`. Only the forked child executes this hook; std retains
/// ownership of cwd, stdio, exec-error reporting, and the child handle.
#[allow(unsafe_code)]
pub fn install_child_signal_reset(command: &mut Command) {
    // SAFETY: pre_exec runs only in the single-threaded child after fork. The
    // closure captures nothing and uses only stack/POD data, a constant list,
    // and async-signal-safe sigemptyset/sigaction/sigprocmask. A zeroed sigaction
    // is valid on macOS/Linux (integer fields and nullable sa_restorer); its
    // mask is explicitly initialized before use and its handler is SIG_DFL.
    // All pointers refer to live stack storage; null old-action/mask pointers
    // request no output. Each error immediately reads thread-local errno via
    // last_os_error, which constructs an inline OS error without allocation.
    // No locks, environment access, formatting, destructors, or panics occur.
    unsafe {
        command.pre_exec(|| {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            if libc::sigemptyset(&mut action.sa_mask) != 0 {
                return Err(io::Error::last_os_error());
            }
            for signal in CHILD_SIGNALS {
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            // Unblock only after every disposition has been reset.
            if libc::sigprocmask(libc::SIG_SETMASK, &action.sa_mask, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// `detached: true` (#552): libuv's child init calls `setsid()`, so the
/// child leads a fresh session — its own process group with no controlling
/// terminal, so commands cannot read the gateway's terminal (`/dev/tty`
/// opens fail ENXIO). Install before `install_child_signal_reset`, which
/// stays the last hook per its own contract; the run order relative to the
/// resets is immaterial (the steps are independent, unlike in libuv where
/// the resets come first). Installers are the spawns whose TS counterparts
/// are detached: the bash runner (with its shared background-task spawn),
/// the ripgrep engine spawn and command hooks. The sidecar and MCP stdio
/// spawns mirror TS spawns that are not detached. Callers that use this
/// must not also request `process_group(0)`: the forked child would already
/// be a group leader and `setsid` would fail with EPERM.
#[allow(unsafe_code)]
pub fn install_child_detach_session(command: &mut Command) {
    // SAFETY: pre_exec runs only in the single-threaded child after fork. The
    // closure captures nothing and calls only setsid, an async-signal-safe
    // syscall with no locks, environment access, allocation, formatting, or
    // destructors. Its error path reports the child's errno through
    // last_os_error, which constructs an inline OS error without allocating.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

"#;

fn child_source_boundary(source: &str) {
    let tokens = rust_tokens(source);
    let mut lint_attributes = Vec::new();
    for (start, token) in tokens.iter().enumerate() {
        if token != "#" {
            continue;
        }
        let mut open = start + 1;
        if tokens.get(open).map(String::as_str) == Some("!") {
            open += 1;
        }
        if tokens.get(open).map(String::as_str) != Some("[") {
            continue;
        }
        let mut depth = 1;
        let mut end = open + 1;
        while depth != 0 {
            match tokens
                .get(end)
                .map(String::as_str)
                .expect("closed attribute")
            {
                "[" => depth += 1,
                "]" => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        if tokens[start..end]
            .iter()
            .any(|token| token == "unsafe_code")
        {
            lint_attributes.push((start, end));
        }
    }
    assert_eq!(
        lint_attributes.len(),
        3,
        "exactly one unsafe_code lint attribute per approved function"
    );
    // The approved functions' full extents: the lint attribute through the
    // closing brace of the body. `bodies` holds `(body_open, body_close)`
    // half-open token ranges for the unsafe-block binding below.
    let mut functions = Vec::new();
    for ((start, end), name) in lint_attributes.iter().zip([
        "ignore_sigxfsz",
        "install_child_signal_reset",
        "install_child_detach_session",
    ]) {
        assert_eq!(
            &tokens[*start..*end],
            ["#", "[", "allow", "(", "unsafe_code", ")", "]"]
        );
        assert_eq!(&tokens[*end..*end + 4], ["pub", "fn", name, "("]);
        // Skip the balanced parameter list to the body's opening brace.
        let mut depth = 0;
        let mut index = *end + 3;
        loop {
            match tokens
                .get(index)
                .map(String::as_str)
                .expect("closed parameter list")
            {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        // An optional return type (`-> io::Result<()>`) sits between the
        // parameter list and the body's opening brace; skip it.
        let mut body_open = index + 1;
        if tokens.get(body_open).map(String::as_str) == Some("-") {
            assert_eq!(tokens[body_open + 1], ">");
            while tokens[body_open].as_str() != "{" {
                body_open += 1;
            }
        }
        assert_eq!(tokens[body_open], "{", "approved function body");
        let mut depth = 1;
        let mut body_close = body_open + 1;
        while depth != 0 {
            match tokens
                .get(body_close)
                .map(String::as_str)
                .expect("closed function body")
            {
                "{" => depth += 1,
                "}" => depth -= 1,
                _ => {}
            }
            body_close += 1;
        }
        functions.push((*start, body_open, body_close));
    }
    let unsafe_blocks: Vec<(usize, usize)> = tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.as_str() == "unsafe")
        .map(|(start, _)| {
            assert_eq!(tokens[start + 1], "{");
            let mut depth = 1;
            let mut end = start + 2;
            while depth != 0 {
                match tokens
                    .get(end)
                    .map(String::as_str)
                    .expect("closed unsafe block")
                {
                    "{" => depth += 1,
                    "}" => depth -= 1,
                    _ => {}
                }
                end += 1;
            }
            (start, end)
        })
        .collect();
    assert_eq!(
        unsafe_blocks.len(),
        3,
        "exactly one unsafe keyword per approved function"
    );
    // Bind every unsafe block to an approved function body: each block must
    // lie inside exactly one approved body, and each approved body must own
    // exactly one block. Moving a block across functions (reset=2, detach=0)
    // or adding a copy fails here even before the full-file pin below.
    for (start, _) in &unsafe_blocks {
        let owners = functions
            .iter()
            .filter(|(_, body_open, body_close)| *start > *body_open && *start < *body_close)
            .count();
        assert_eq!(
            owners, 1,
            "unsafe block outside the approved function bodies"
        );
    }
    for (_, body_open, body_close) in &functions {
        let owned = unsafe_blocks
            .iter()
            .filter(|(start, _)| *start > *body_open && *start < *body_close)
            .count();
        assert_eq!(
            owned, 1,
            "each approved function body owns exactly one unsafe block"
        );
    }
    // Pin the entire file's token sequence (comments stripped). Pinning only
    // the unsafe blocks left the surrounding function body editable: a local
    // `AliasLibc::setsid()` plus `type libc = AliasLibc` kept the pinned
    // block's tokens byte-identical while redirecting the actual call.
    assert_eq!(
        tokens,
        rust_tokens(CHILD_SOURCE),
        "pilotdeck-child-signals/src/lib.rs changed; updating CHILD_SOURCE requires extending the #386/#552/#621 human-approved unsafe exception"
    );
    assert!(source.contains("#![cfg(unix)]"));
    for forbidden in ["macro_rules", "macro", "include", "mod"] {
        assert!(
            !tokens.iter().any(|token| token == forbidden),
            "child crate contains {forbidden}"
        );
    }
    for (index, token) in tokens.iter().enumerate() {
        if token == "!"
            && index > 0
            && tokens
                .get(index + 1)
                .is_some_and(|next| ["(", "[", "{"].contains(&next.as_str()))
        {
            assert!(
                !tokens[index - 1].starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_'),
                "child crate must not invoke macros"
            );
        }
    }
}

// These are the only existing exceptions. Keep the toolchain hash in FROZEN.md.
// Changing transport/build.rs requires updating this hash and review confirming
// it emits no cargo:rustc-flags, cargo:rustc-env=RUSTC*, --cap-lints, or other
// directives that can affect lint enforcement. Its compilation inherits forbid.
fn registered_build_files(root: &Path) {
    let frozen = fs::read_to_string(root.join("specs/ui-server-parity/FROZEN.md")).unwrap();
    let hashes: Vec<_> = frozen
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() == 2 && fields[1] == "../../rust-toolchain.toml").then(|| fields[0])
        })
        .collect();
    assert_eq!(hashes.len(), 1, "unique frozen root toolchain registration");
    for (relative, expected) in [
        ("rust-toolchain.toml", hashes[0]),
        (
            "src/pilotdeck-transport/build.rs",
            "f55927d26226112d4362567deec12002855906c15e53c4fbf1f2e80a9458cd56",
        ),
    ] {
        let path = root.join(relative);
        assert!(
            fs::symlink_metadata(&path).unwrap().file_type().is_file(),
            "registered build file must be a regular file: {relative}"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
            expected,
            "registered build file changed: {relative}"
        );
    }
}

// Check names rather than contents, including empty and unstaged files.
fn assert_build_configuration_path(root: &Path, path: &Path) {
    let name = path.file_name().unwrap().to_str().unwrap();
    let kind = fs::symlink_metadata(path).unwrap().file_type();
    let registered = path == root.join("rust-toolchain.toml")
        || path == root.join("src/pilotdeck-transport/build.rs");
    assert!(
        registered
            || (name != ".cargo"
                && !name.starts_with("rust-toolchain")
                && name != "build.rs"
                && (kind.is_dir() || !["config", "config.toml"].contains(&name))),
        "repository Cargo configuration/toolchain paths can replace the compiler or override lint policy and are forbidden: {}",
        path.display()
    );
}

fn build_configuration_boundary(root: &Path, directory: &Path) {
    for name in entries(directory) {
        let path = directory.join(&name);
        assert_build_configuration_path(root, &path);
        if [
            ".git",
            "_build",
            "_tmp",
            "target",
            "node_modules",
            "resources",
            "vault",
            "notes",
        ]
        .contains(&name.as_str())
        {
            // Large trees skip recursion only; their immediate children still
            // obey the same name policy (in particular _build/.cargo).
            // Git metadata is outside the repository source/config tree;
            // its own .git/config is not a Cargo input.
            if name != ".git" && fs::symlink_metadata(&path).unwrap().is_dir() {
                for child in entries(&path) {
                    assert_build_configuration_path(root, &path.join(child));
                }
            }
            continue;
        }
        if fs::symlink_metadata(&path).unwrap().is_dir() {
            build_configuration_boundary(root, &path);
        }
    }
}

fn cargo_configuration_locations_boundary(manifest: &Path) {
    // Cargo discovers config from the build cwd through all ancestors, even
    // above a nested worktree. Enforce the supported workspace manifest tree
    // and inspect the actual cwd chain, including directories below manifest.
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert!(
        cwd.starts_with(manifest.canonicalize().unwrap()),
        "guard cwd must be within the workspace manifest tree: {} (manifest {})",
        cwd.display(),
        manifest.display()
    );
    for directory in cwd.ancestors() {
        for name in ["config", "config.toml"] {
            let path = directory.join(".cargo").join(name);
            assert!(
                fs::symlink_metadata(&path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                "ancestor Cargo configuration can replace the compiler or override lint policy and is forbidden: {}. For this guard test only, use a checkout whose manifest ancestors have no .cargo/config or .cargo/config.toml",
                path.display()
            );
        }
    }
    let cargo_home = std::env::var_os("CARGO_HOME").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").expect("HOME required when CARGO_HOME is unset"))
                .join(".cargo")
        },
        PathBuf::from,
    );
    match fs::read_dir(&cargo_home) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.expect("read Cargo home entry");
                assert!(
                    !entry.file_name().to_string_lossy().starts_with("config"),
                    "Cargo home config* can replace the compiler or override lint policy and is forbidden: {}. For this guard test only, set CARGO_HOME to a config-free Cargo home with dependencies cached, or run the compiled guard binary with that CARGO_HOME",
                    entry.path().display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!(
            "cannot inspect Cargo home {}: {error}",
            cargo_home.display()
        ),
    }
}

fn compiler_environment_boundary() {
    for (name, value) in std::env::vars_os() {
        let key = name.to_string_lossy();
        let compiler_override = [
            "RUSTFLAGS",
            "RUSTDOCFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
        ]
        .contains(&key.as_ref())
            || key.starts_with("CARGO_BUILD_")
            || (key.starts_with("CARGO_TARGET_")
                && ["_RUSTFLAGS", "_LINKER", "_RUNNER"]
                    .iter()
                    .any(|suffix| key.ends_with(suffix)));
        assert!(
            !compiler_override || value.is_empty(),
            "{key} can replace the compiler, alter compilation/linking, or bypass the lint policy; nonempty values are forbidden. For this guard test only, run: env -u {key} cargo test -p pilotdeck-gateway --test unsafe_boundary -- --exact compiler_enforces_gateway_boundary"
        );
    }
}

#[test]
fn compiler_enforces_gateway_boundary() {
    let gateway = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = gateway.parent().unwrap();
    compiler_environment_boundary();
    cargo_configuration_locations_boundary(workspace);
    let root = workspace.parent().unwrap();
    registered_build_files(root);
    build_configuration_boundary(root, root);
    let workspace_manifest = fs::read_to_string(workspace.join("Cargo.toml")).unwrap();
    assert_eq!(
        section(&workspace_manifest, "[workspace.lints.rust]"),
        ["unsafe_code = \"forbid\""]
    );
    let members = workspace_paths(&workspace_manifest, "members");
    assert_eq!(
        members
            .iter()
            .filter(|member| *member == "pilotdeck-child-signals")
            .count(),
        1
    );
    for member in &members {
        let manifest = fs::read_to_string(workspace.join(member).join("Cargo.toml")).unwrap();
        assert!(
            !section(&manifest, "[package]")
                .iter()
                // Fail closed on quoted/escaped keys as well: TOML can spell
                // the build key as "build" or with Unicode escapes.
                .any(|line| line.starts_with("build") || line.starts_with(['"', '\''])),
            "custom build scripts or quoted package keys require extending the lint guard: {member}"
        );
        assert!(
            member == "pilotdeck-transport"
                || fs::symlink_metadata(workspace.join(member).join("build.rs"))
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
            "only the hash-checked transport build.rs is allowed: {member}"
        );
        let lint_tables: Vec<_> = stripped_lines(&manifest)
            .into_iter()
            .filter(|line| line.starts_with("[lints"))
            .collect();
        if member == "pilotdeck-child-signals" {
            let tables: Vec<_> = stripped_lines(&manifest)
                .into_iter()
                .filter(|line| line.starts_with('['))
                .collect();
            assert_eq!(
                tables,
                [
                    "[package]",
                    "[lints.rust]",
                    "[lints.clippy]",
                    "[dependencies]"
                ],
                "child manifest table whitelist"
            );
            assert_eq!(
                section(&manifest, "[package]"),
                [
                    "name = \"pilotdeck-child-signals\"",
                    "version.workspace = true",
                    "edition.workspace = true",
                    "rust-version.workspace = true",
                    "license.workspace = true",
                    "publish.workspace = true",
                ],
                "child package whitelist (the existing seven lines including [package])"
            );
            assert_eq!(lint_tables, ["[lints.rust]", "[lints.clippy]"], "{member}");
            assert_eq!(
                section(&manifest, "[lints.rust]"),
                ["unsafe_code = \"deny\""],
                "{member}"
            );
            assert_eq!(
                section(&manifest, "[lints.clippy]"),
                section(&workspace_manifest, "[workspace.lints.clippy]"),
                "{member}"
            );
            assert_eq!(
                section(&manifest, "[dependencies]"),
                ["libc = \"=0.2.190\""]
            );
        } else {
            // Frozen rustc 1.99.0 (b940084d7, 2026-09-28) rejects source
            // allow(unsafe_code) over Cargo's --forbid=unsafe_code with E0453:
            // direct crate attribute and macro_rules! expansions of both
            // outer function and inner module attributes were tested.
            // Raw probes: _tmp/20261021-issue-386/RESULT3.md, 修复轮 10.
            assert_eq!(
                lint_tables,
                ["[lints]"],
                "{member} must inherit workspace lints"
            );
            assert_eq!(
                section(&manifest, "[lints]"),
                ["workspace = true"],
                "{member}"
            );
        }
    }
    resolved_dependency_boundary(workspace, &members);
    let mut excludes = workspace_paths(&workspace_manifest, "exclude");
    excludes.sort();
    let mut expected_excludes = VENDORED_PACKAGES;
    expected_excludes.sort();
    assert_eq!(excludes, expected_excludes, "workspace exclude whitelist");

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
    child_source_boundary(&fs::read_to_string(child.join("src/lib.rs")).unwrap());
}
