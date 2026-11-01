//! 守卫边界：检查 workspace 成员源码的 lint 继承与 manifest、child crate
//! 内容、全部 workspace 成员的解析后依赖闭包及其所有声明依赖的 source/path
//! （含 dev/build/target 与未启用的可选依赖）、仓内 Cargo 配置与 toolchain 文件、
//! workspace manifest 树内的测试 cwd 到根的 Cargo 配置、CARGO_HOME（未设时 ~/.cargo）的
//! config*、测试进程可见的编译相关环境变量（#448 补 RUSTDOC / RUSTC_BOOTSTRAP；
//! RUSTUP_TOOLCHAIN 仅在 RUSTUP_TOOLCHAIN_SOURCE=toolchain-file 且值等于本仓
//! rust-toolchain.toml 钉住 channel、或 channel 加上运行中 cargo 报告的真实
//! host triple 时放行），以及 #448 在 Linux 上经 /proc 读到的真实调用上下文：
//! 父进程（cargo / cargo-nextest 本尊）的 cwd 到根的 Cargo 配置链、其 argv
//! 与祖先链上每个名为 cargo 的进程的 argv 里的 `--config` 覆盖及其 cwd 到根
//! 的 Cargo 配置链（cb8 实测
//! cargo 会 exec 成外部子命令：真跑 `cargo nextest run` 时 cargo 本尊已不
//! 存在、其 argv 随之消失，归头注登记的人工评审边界；祖先扫描覆盖 cargo
//! 存活的一切布局，如 `cargo test` 派生的 harness-free runner 再拉起本守
//! 卫的两层布局）、其进程像与 CARGO 环境的一致性（防树外 config 用
//! `[env] force = true` 把守卫的 cargo metadata 审计指向假 cargo）。
//! vendored 包（`vendor/tiktoken-rs`、`data/icu_collator_data`）不在 workspace
//! lint 继承内，因此其声明/源码包含边与成员同受检查：path 依赖（含 dev/build/
//! target 与未启用的可选依赖）、build script、proc-macro 及其它非 lib target、
//! `[lib] path`，以及从 `[lib] src_path` 出发沿 `#[path = ...]` / `include!("...")`
//! 字面量目标做传递闭包得到的全部编译源（不按扩展名过滤，`.rs.data` 与无扩展名
//! 文件同样扫描）；闭包中的每个目标都必须留在该 vendored 包目录内、是文件、
//! 不是 symlink。扫描面是 rustc 的 fail-closed 超集：目录下所有 `.rs` 文件，以及
//! 所有能被 Rust 词法解析、且 token 序列里出现 `#[path]` / `#[r#path]` /
//! `#![path]` / `#![r#path]` / `cfg_attr(..., path|r#path = ...)` 或 `include!`
//! 的常规文件，不论扩展名。
//! 词法统一用 `proc_macro2::TokenStream::from_str`：注释、raw string（含
//! `br"..."` / `cr"..."` 前缀）与字面量边界由它判定，本文件不再手写扫描器。
//! UTF-8 解码失败的二进制数据不会被 rustc 当源码编译，跳过；非 `.rs` 文件
//! 词法失败时，只有「去空白原文」与「朴素去注释后再去空白」两份文本都不含
//! `#[path` / `#![path` / `include!` / `cfg_attr(...path=...)` 这类真实边 marker
//! 才视为普通文本，否则 fail-closed（取并集：注释混淆靠第二份，`"http://..."`
//! 截断靠第一份）。`.rs` 文件和候选非 `.rs` 文件词法失败一律拒绝。
//! `#[path]` 只允许出现在文件顶层：直接按 `Group` 嵌套判断，任何非顶层
//! `#[path]`（宏体、`mod $i { ... }`、inline module、函数体、宏调用实参）一律
//! 拒绝，因为其基准目录取决于展开上下文，与本扫描器的「文件目录 + 字面量」
//! 不一致，否则会审到诱饵文件而漏掉真正编译的文件。doc 注释会被 proc-macro2
//! 转成 `#[doc = "..."]`，只认 `doc` 以外的属性名，doc 文本里的 `#[path]`
//! 只是字符串，不构成边。`macro_rules!` 定义体内的 `include!` 同样拒绝：展开
//! 处与定义处文件可能不同，基准不可静态确定。字面量只接受不含反斜杠的普通
//! `"..."` 和 `r#*"..."#*`；带转义、`b"` / `br"` / `c"` / `cr"` 以及带后缀的
//! 字面量一律拒绝。vendored 包内部的 unsafe 本身仍不属于本守卫范围。
//! vendored 源码内容不做哈希 pin：内容本身是否可信属于冻结件/人工评审边界；
//! 本守卫只锁“声明边与源码包含边不能引出该目录”。
//! #448 登记的剩余边界（做不到的部分，明确写出）：测试进程 cwd 恒为包根，
//! 没有 /proc 的平台（macOS / Windows）读不到父进程的 cwd / argv / 进程像，
//! 那些平台上的树外调用目录 `.cargo/config*`、`cargo --config` 与
//! `[env]` CARGO 覆盖不可核实——静默放行等于假 PASS，因此非 Linux 默认测试
//! 失败并说明原因，仅显式设置 `PDRS_UNSAFE_BOUNDARY_ALLOW_NON_LINUX=1`
//! 时以可辨认的 stderr skip 行放行（skip 行经 stderr 句柄直写，不被
//! libtest 捕获，默认 `cargo test` 输出即可见；CI 全是 ubuntu 作业，无需
//! 设置）；初始父进程即使就是 pid 1（容器入口即 cargo 的布局）也照常核
//! 验，只有向上走才在 init 之前停止，`/proc/<pid>/exe`（或 stat）读失败时
//! 区分「进程
//! 已退出」（停止）与「无权限等其它错误」（stderr 打 note 后停止，到达
//! root 属主的合法祖先如 sshd 即此情形，再往上的 cargo 不可核实，与 CI
//! 脚本替换编译器同归人工评审）；`RUSTUP_HOME`（rustup 会为子进程导出，
//! cb8 实测）可把 toolchain 解析重定向到假 home，但假 toolchain 名仍须
//! 精确等于 channel 或 channel-<真实 host triple>，等价于替换 rustup
//! 状态本身，同归人工评审；直接运行已编译守卫二进制（祖先里没有名为
//! cargo 的进程）时进程像一致性无法核实，仅当祖先里出现名为 cargo 的
//! 进程时才比对 CARGO（nextest 自身的 `--config-file` 是另一个 token，
//! 不受 `--config` 检查影响）。
//! CI yml 与脚本自行替换编译器（包括 source .env、设置 CARGO_HOME、RUSTC /
//! CARGO 包装器）由人工评审负责，不靠扫描脚本文本覆盖；依据：
//! state/decisions-20261021.md「卡 · #386 守卫的构建配置检查改为「一律不许存在」，并划清守卫边界（AI 代批，待人复核）」。
//! source 只接受 null（已审计本地包）与 registry+；git 等其它 kind 一律拒绝。
//! registry 依赖内部 unsafe 不属于本守卫的审计范围；registry 依赖由 Cargo.lock
//! pin 评审；vendored 内容的变更仍须按冻结件规则评审。

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[allow(dead_code)]
#[path = "common/source_inputs.rs"]
mod source_inputs;
use serde_json::Value;
use sha2::{Digest, Sha256};
use source_inputs::*;

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

// Package declarations include optional and target-specific edges even when
// they are absent from the active resolve graph. Cargo normalizes
// inline/table/workspace-inherited dependency syntax, so filtering the
// metadata `dependencies` array is the declaration-level check for both
// workspace members and vendored packages.
fn declared_local_dependency_boundary(
    package: &Value,
    description: &str,
    allowed: &BTreeSet<PathBuf>,
) {
    for dependency in package["dependencies"].as_array().unwrap() {
        let name = dependency["name"].as_str().unwrap();
        if is_local_source(
            dependency
                .get("source")
                .expect("declared dependency source"),
            &format!("{description} dependency {name}"),
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
                allowed.contains(&directory),
                "unknown declared path dependency of {description}: {name} ({})",
                directory.display()
            );
        }
    }
}

fn collect_vendored_source_candidates(package: &str, directory: &Path, files: &mut Vec<PathBuf>) {
    for name in entries(directory) {
        let path = directory.join(&name);
        let kind = fs::symlink_metadata(&path).unwrap().file_type();
        assert!(
            !kind.is_symlink(),
            "vendored package {package} source tree contains a symlink: {}",
            path.display()
        );
        if kind.is_dir() {
            collect_vendored_source_candidates(package, &path, files);
        } else if kind.is_file()
            && (path.extension().and_then(|extension| extension.to_str()) == Some("rs")
                || vendored_source_mentions_edge(package, &path))
        {
            files.push(path);
        }
    }
}

// Scan the transitive closure of literal `#[path]` / `include!` targets that
// starts at the `[lib]` source. Extensions carry no meaning to rustc here:
// `.rs.data` and extensionless files are compiled exactly like `.rs`, so every
// target is scanned regardless of its name. The recursive sweep over `.rs`
// files and every file naming an edge is kept as a fail-closed superset (an
// unreferenced Rust file must not introduce an escaping edge either), but it
// is not the only seed.
fn vendored_compile_closure(package: &str, directory: &Path, lib_sources: Vec<PathBuf>) {
    let mut pending = lib_sources;
    collect_vendored_source_candidates(package, directory, &mut pending);
    let mut seen = HashSet::new();
    while let Some(file) = pending.pop() {
        let file = file.canonicalize().unwrap_or_else(|error| {
            panic!(
                "vendored package {package} source cannot be resolved: {} ({error})",
                file.display()
            )
        });
        if !seen.insert(file.clone()) {
            continue;
        }
        pending.extend(vendored_source_edges(package, directory, &file));
    }
}

// A vendored package sits outside `[workspace.lints]`, so any build script,
// proc-macro, non-lib target, or source include that leaves the vendored
// directory would be compiled/executed without the workspace `forbid`. Keep
// the reviewed surface to the vendored lib source tree, read through its
// literal `#[path]` / `include!` graph.
fn vendored_package_boundary(
    package: &str,
    metadata: &Value,
    directory: &Path,
    allowed: &BTreeSet<PathBuf>,
) {
    declared_local_dependency_boundary(metadata, &format!("vendored package {package}"), allowed);
    let targets = metadata["targets"]
        .as_array()
        .expect("vendored package targets");
    let mut lib_sources = Vec::new();
    for target in targets {
        let kinds: Vec<_> = target["kind"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| kind.as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            ["lib"],
            "vendored package {package} must expose only a lib target; build scripts, proc-macros, bins, examples, tests and benches are forbidden"
        );
        let raw_source = Path::new(target["src_path"].as_str().unwrap());
        let kind = fs::symlink_metadata(raw_source)
            .expect("vendored lib source")
            .file_type();
        assert!(
            !kind.is_symlink(),
            "vendored package {package} lib source is a symlink: {}",
            raw_source.display()
        );
        let source = raw_source.canonicalize().expect("vendored lib source");
        assert!(
            source.starts_with(directory),
            "vendored package {package} lib source escapes its package directory: {}",
            source.display()
        );
        assert!(
            source.is_file(),
            "vendored package {package} lib source is not a file: {}",
            source.display()
        );
        lib_sources.push(source);
    }
    vendored_compile_closure(package, directory, lib_sources);
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

    let workspace_root = workspace.canonicalize().unwrap();
    let vendored: BTreeSet<_> = VENDORED_PACKAGES
        .iter()
        .map(|relative| {
            let path = workspace.join(relative);
            let kind = fs::symlink_metadata(&path)
                .unwrap_or_else(|error| {
                    panic!(
                        "vendored package path is missing: {} ({error})",
                        path.display()
                    )
                })
                .file_type();
            assert!(
                kind.is_dir(),
                "vendored package must be a real directory, not a symlink or file: {}",
                path.display()
            );
            let directory = path.canonicalize().unwrap();
            assert!(
                directory.starts_with(&workspace_root),
                "vendored package escapes the workspace: {} -> {}",
                path.display(),
                directory.display()
            );
            directory
        })
        .collect();
    let mut declared_allowed = checked_members.clone();
    declared_allowed.extend(vendored.iter().cloned());
    // `cargo metadata` only lists packages reachable from the workspace. Both
    // vendored packages are in the graph today (tiktoken via the tokenizer,
    // icu data via `[patch.crates-io]`), but fail closed if either disappears:
    // an unaudited vendored manifest must not silently drop out of the guard.
    for directory in &vendored {
        let metadata = packages
            .values()
            .find(|package| package_directory(package) == *directory)
            .unwrap_or_else(|| {
                panic!(
                    "vendored package is absent from cargo metadata; the guard cannot audit its declaration edges: {}",
                    directory.display()
                )
            });
        let id = metadata["id"].as_str().unwrap();
        assert!(
            !member_ids.contains(id),
            "vendored package must not also be a workspace member: {id}"
        );
        vendored_package_boundary(id, metadata, directory, &declared_allowed);
    }
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
            declared_local_dependency_boundary(
                package,
                &format!("workspace member {id}"),
                &declared_allowed,
            );
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

// Project the one proc-macro2 lexer to identifier/punctuation text for the
// child-source pin. String/char/doc literals are dropped so their text does
// not matter, but numeric literal text stays: the old byte lexer exposed the
// digits of `32`, so dropping them would silently stop the pin from noticing
// a changed signal range. Comments are already gone from the token stream, and
// group delimiters stay punctuation.
fn rust_tokens(source: &str) -> Vec<String> {
    let tokens = lex_source(source).expect("child crate source is lexable Rust");
    let mut projected = Vec::new();
    project_tokens(&tokens, &mut projected);
    projected
}

fn project_tokens(tokens: &[SourceToken], projected: &mut Vec<String>) {
    for token in tokens {
        match token {
            SourceToken::Ident(word) => projected.push(word.clone()),
            SourceToken::Punct(ch) => projected.push(ch.to_string()),
            SourceToken::Literal(text) => {
                if text.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                    projected.push(text.clone());
                }
            }
            SourceToken::Group { delimiter, tokens } => {
                if let Some((open, close)) = delimiter_chars(*delimiter) {
                    projected.push(open.to_string());
                    project_tokens(tokens, projected);
                    projected.push(close.to_string());
                } else {
                    project_tokens(tokens, projected);
                }
            }
        }
    }
}

// The complete expected content of pilotdeck-child-signals/src/lib.rs
// (proc-macro2 strips line/block comments; the projection drops string, char
// and doc literals but keeps numeric literal text, so comment text edits pass
// while a changed number still fails. Adding or removing a doc attribute does
// change the token sequence because its `#[doc = ...]` name/punctuation
// remains. Every other change — imports, type aliases shadowing `libc`, helper
// items, a moved or copied unsafe block — changes the token sequence). ANY
// change to that file must update this copy in the same commit and re-extends
// #386/#552 human-approved unsafe exception; do not edit either side alone.
// Source order: the #386 signal reset, then the #552 setsid.
const CHILD_SOURCE: &str = r#"
//! The #386 human-approved exception: reset inherited child signals before exec.
//! The #552 human-approved addition: libuv's `detached` `setsid()` in the same
//! fork-to-exec window, for every spawn whose TS counterpart is detached (the
//! bash runner, background tasks, the ripgrep engine spawn and command hooks;
//! A6.5-BASH-OS-001 setsid half).
#![cfg(unix)]

use std::{io, os::unix::process::CommandExt, process::Command};

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
        2,
        "exactly one unsafe_code lint attribute per approved function"
    );
    // The approved functions' full extents: the lint attribute through the
    // closing brace of the body. `bodies` holds `(body_open, body_close)`
    // half-open token ranges for the unsafe-block binding below.
    let mut functions = Vec::new();
    for ((start, end), name) in lint_attributes
        .iter()
        .zip(["install_child_signal_reset", "install_child_detach_session"])
    {
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
        let body_open = index + 1;
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
        2,
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
        "pilotdeck-child-signals/src/lib.rs changed; updating CHILD_SOURCE requires extending the #386/#552 human-approved unsafe exception"
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

// #448: one rejection for every directory chain that Cargo config discovery
// walks. The boundary text names the origin so a rejection explains which
// chain carried the configuration; anything but a definite NotFound fails
// closed.
fn assert_no_ancestor_cargo_configuration(chain: &Path, boundary: &str, remedy: &str) {
    for directory in chain.ancestors() {
        for name in ["config", "config.toml"] {
            let path = directory.join(".cargo").join(name);
            assert!(
                fs::symlink_metadata(&path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                "{boundary} can replace the compiler or override lint policy and is forbidden: {}. {remedy}",
                path.display()
            );
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
    assert_no_ancestor_cargo_configuration(
        &cwd,
        "ancestor Cargo configuration",
        "For this guard test only, use a checkout whose manifest ancestors have no .cargo/config or .cargo/config.toml",
    );
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
                    "Cargo home config* can replace the compiler or override lint policy and is forbidden: {}. For this guard test only, set CARGO_HOME to a config-free Cargo home with dependencies cached, or run the compiled guard binary with that CARGO_HOME and CARGO naming the cargo executable",
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

// #448: the toolchain-selection family rounds out the compiler overrides.
// RUSTDOC swaps the documentation compiler and RUSTC_BOOTSTRAP unlocks
// unstable rustc options on the pinned stable channel; neither may be set to
// a nonempty value. RUSTUP_TOOLCHAIN is the one toolchain variable the normal
// flow itself sets: rustup's shim resolves this repository's hash-pinned
// rust-toolchain.toml and exports RUSTUP_TOOLCHAIN with
// RUSTUP_TOOLCHAIN_SOURCE=toolchain-file (probed on rustup 1.29.2 / cb8: the
// toolchain file exports the toolchain-file source, `RUSTUP_TOOLCHAIN=x
// cargo …` exports env, and `cargo +x …` exports cli), so exactly that
// combination passes, with the resolved name equal to the pinned channel or
// that channel plus the real host triple the running cargo reports — an
// arbitrary channel-anystring suffix would accept a linked fake toolchain
// name. +toolchain CLI, a user-exported RUSTUP_TOOLCHAIN, and a linked
// non-host toolchain are all rejected.
fn toolchain_environment_boundary(root: &Path) {
    for key in ["RUSTDOC", "RUSTC_BOOTSTRAP"] {
        assert!(
            std::env::var_os(key).is_none_or(|value| value.is_empty()),
            "#448 {key} can swap the documentation compiler or unlock unstable rustc options on the pinned stable channel, and is not covered by the toolchain file pin; nonempty values are forbidden. For this guard test only, run: env -u {key} cargo test -p pilotdeck-gateway --test unsafe_boundary -- --exact compiler_enforces_gateway_boundary"
        );
    }
    if let Some(toolchain) = std::env::var_os("RUSTUP_TOOLCHAIN") {
        let toolchain = toolchain.to_string_lossy();
        let source = std::env::var_os("RUSTUP_TOOLCHAIN_SOURCE")
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        let manifest = fs::read_to_string(root.join("rust-toolchain.toml"))
            .expect("#448 pinned rust-toolchain.toml is readable");
        let channel = section(&manifest, "[toolchain]")
            .iter()
            .find_map(|line| line.strip_prefix("channel = "))
            .and_then(|value| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
            .expect("#448 pinned toolchain channel is declared");
        // A rustup shim re-resolves RUSTUP_TOOLCHAIN itself, so the probe
        // removes it: the shim then resolves this repository's toolchain file
        // from the guard's cwd (the package root) while the real cargo binary
        // ignores the variable anyway. A missing CARGO or a failing probe
        // leaves no verified host triple and fails the assertion below.
        let host = std::env::var_os("CARGO").and_then(|cargo| {
            Command::new(cargo)
                .arg("-vV")
                .env_remove("RUSTUP_TOOLCHAIN")
                .env_remove("RUSTUP_TOOLCHAIN_SOURCE")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| {
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .find_map(|line| line.strip_prefix("host: "))
                        .map(str::to_owned)
                })
        });
        assert!(
            source == "toolchain-file"
                && (toolchain.as_ref() == channel
                    || host.as_deref()
                        .is_some_and(|host| toolchain == format!("{channel}-{host}"))),
            "#448 RUSTUP_TOOLCHAIN={toolchain} (RUSTUP_TOOLCHAIN_SOURCE={source}) does not come from this repository's pinned rust-toolchain.toml (channel {channel}{host_note}); the unsafe_code=forbid guarantee is defined for the pinned rustc only. For this guard test only, run cargo from inside this checkout without +toolchain, an exported RUSTUP_TOOLCHAIN, or a rustup override (see `rustup override unset`)",
            host_note = host.as_deref().map_or_else(
                || String::from(
                    "; the running cargo did not report its host triple, so channel-host names cannot be verified"
                ),
                |host| format!(", which resolves on this host as {channel}-{host}"),
            )
        );
    }
}

#[cfg(target_os = "linux")]
fn parent_process_id(pid: u32) -> std::io::Result<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // The comm field may contain ')' and spaces; the fixed fields resume
    // after the final closing parenthesis: state, then ppid.
    stat.rsplit(')')
        .next()
        .and_then(|fields| fields.split_whitespace().nth(1))
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unparsable ppid field in /proc/{pid}/stat"),
            )
        })
}

#[cfg(target_os = "linux")]
fn read_cmdline(pid: u32) -> std::io::Result<Vec<String>> {
    let cmdline = fs::read(format!("/proc/{pid}/cmdline"))?;
    Ok(cmdline
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument).into_owned())
        .collect())
}

// Cargo's own options end at the first `--`; later tokens belong to the test
// harness, which has a `--config` flag of its own that must not be mistaken
// for a cargo override.
#[cfg(target_os = "linux")]
fn assert_no_config_cli_overrides(argv: &[String]) {
    let cargo_options = argv
        .split(|argument| argument == "--")
        .next()
        .unwrap_or(&[]);
    for argument in cargo_options {
        assert!(
            argument != "--config" && !argument.starts_with("--config="),
            "#448 cargo --config overrides (build.rustc, build.rustflags, target tables, [env]) are not observable through files or the test environment and are forbidden: {argv:?}. For this guard test only, run cargo without --config"
        );
    }
}

// #448: the test process cwd is always the package root, so walking the cwd
// ancestors can never see where cargo itself was invoked from — and Cargo
// discovers .cargo/config* from that invocation directory, not from the
// manifest tree. On Linux the immediate parent is the runner that spawned
// this binary (cargo itself, or cargo-nextest forwarding cargo's context),
// and /proc exposes exactly the inputs that were effective for the
// invocation: its cwd carries the out-of-tree configuration discovery chain,
// its argv shows the otherwise unobservable `--config` overrides, and its
// process image is the real cargo that a `[env] force=true` CARGO override
// would have to impersonate. Under `cargo nextest run` the direct parent is
// cargo-nextest; cargo exec-replaces into external subcommands (probed on
// cb8 / cargo 1.99.1), so by test time the cargo process itself is gone and
// its argv with it — that absence is the declared manual-review boundary in
// the module header. The ancestor walk (to init, with a truncation note when
// a step is unreadable) covers every layout where a cargo-named process IS
// alive (plain `cargo test`, and runners that a test target spawns), reading
// each one's argv.
#[cfg(target_os = "linux")]
fn cargo_invocation_boundary() {
    let ppid = parent_process_id(std::process::id())
        .expect("#448 the parent runner process must be identifiable through /proc");
    let argv = read_cmdline(ppid).expect("#448 parent runner cmdline must be readable");
    assert_no_config_cli_overrides(&argv);
    let cwd = fs::read_link(format!("/proc/{ppid}/cwd"))
        .expect("#448 parent runner working directory must be readable");
    assert_no_ancestor_cargo_configuration(
        &cwd,
        "#448 cargo invocation directory Cargo configuration (the test process cwd is always the package root and cannot see it)",
        "For this guard test only, invoke cargo from a directory whose ancestors have no .cargo/config or .cargo/config.toml",
    );
    // The runner image identifies the real cargo behind the CARGO variable;
    // an out-of-tree `[env] force=true` CARGO override would otherwise point
    // this guard's own `cargo metadata` audit at a fake cargo. Plain
    // `cargo test` exposes the cargo as the parent; under `cargo nextest run`
    // no cargo is alive at all (cargo exec-replaces into the subcommand).
    // With no cargo ancestry (direct execution of the compiled guard binary,
    // or any exec'd-subcommand runner such as cargo-nextest) the identity
    // cannot be verified and stays within the declared manual-review
    // boundary; with any cargo ancestry visible, CARGO must name one of
    // those ancestors.
    let cargo = PathBuf::from(
        std::env::var_os("CARGO").expect(
            "#448 the guard environment must name its cargo through CARGO; when running the compiled guard binary directly (e.g. the CARGO_HOME remedy), export CARGO=<cargo executable> first",
        ),
    );
    let mut saw_cargo = false;
    let mut verified = false;
    // The starting pid is examined even when it is init (pid 1): in a
    // container whose entrypoint is cargo, the guard's direct parent is
    // pid 1 itself, and skipping it would silently drop the identity and
    // --config audits for exactly that layout. Only the upward walk stops
    // before init (next > 1 below).
    let mut pid = ppid;
    loop {
        let exe = match fs::read_link(format!("/proc/{pid}/exe")) {
            Ok(exe) => exe,
            // The process exited before the guard could read it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                stderr_boundary_line(&format!(
                    "#448 note: the ancestry walk stopped at pid {pid}: /proc/{pid}/exe cannot be read ({error}); any cargo above it stays unverified (declared manual-review boundary)\n"
                ));
                break;
            }
        };
        if exe.file_name().and_then(|name| name.to_str()) == Some("cargo") {
            saw_cargo = true;
            verified |= Path::new(&cargo)
                .canonicalize()
                .is_ok_and(|cargo| cargo == exe);
            let runner_argv = match read_cmdline(pid) {
                Ok(argv) => argv,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => panic!(
                    "#448 the cargo ancestor's cmdline must be readable to audit its --config overrides (pid {pid}): {error}"
                ),
            };
            assert_no_config_cli_overrides(&runner_argv);
            // The cargo ancestor's own invocation directory is on no cwd
            // chain the direct-parent walk can see: cargo discovers
            // .cargo/config* from the directory it is invoked in, while the
            // runner below it (the guard's direct parent) runs from the
            // package root.
            let runner_cwd = match fs::read_link(format!("/proc/{pid}/cwd")) {
                Ok(cwd) => cwd,
                // The process exited before the guard could read it.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => panic!(
                    "#448 the cargo ancestor's working directory must be readable to audit its invocation directory Cargo configuration (pid {pid}): {error}"
                ),
            };
            assert_no_ancestor_cargo_configuration(
                &runner_cwd,
                "#448 cargo ancestor invocation directory Cargo configuration (the direct parent's cwd chain cannot see where that cargo was invoked from)",
                "For this guard test only, invoke every cargo ancestor from a directory whose ancestors have no .cargo/config or .cargo/config.toml",
            );
        }
        match parent_process_id(pid) {
            Ok(next) if next > 1 && next != pid => pid = next,
            Ok(_) => break,
            // The process exited before the guard could read it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                stderr_boundary_line(&format!(
                    "#448 note: the ancestry walk stopped at pid {pid}: /proc/{pid}/stat cannot be read ({error}); any cargo above it stays unverified (declared manual-review boundary)\n"
                ));
                break;
            }
        }
    }
    assert!(
        !saw_cargo || verified,
        "#448 the CARGO executable in the guard environment does not match any cargo process in this run's ancestry; an out-of-tree [env] override can point the cargo metadata audit at a fake cargo: {}",
        cargo.display()
    );
}

// #448: the acknowledgement must be exactly "1" — unset, empty, "0", or any
// other value refuses, so a typo or an inherited empty value cannot silently
// acknowledge the boundary. Pure and unit-tested on every platform because
// the non-Linux branch it gates cannot run under CI's ubuntu hosts.
fn acknowledges_non_linux_boundary(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

// #448 boundary lines must reach default `cargo test` output: libtest
// captures the eprintln! macro for a passing test, so skip and note lines
// are written through the stderr handle directly (probed on rustc 1.99.1:
// handle writes bypass the capture; the macro's output does not).
fn stderr_boundary_line(text: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(text.as_bytes());
    let _ = std::io::stderr().flush();
}

#[test]
fn non_linux_acknowledgement_gate() {
    assert!(acknowledges_non_linux_boundary(Some(std::ffi::OsStr::new(
        "1"
    ))));
    assert!(!acknowledges_non_linux_boundary(None));
    for rejected in ["0", "", "true", "1 ", " 1"] {
        assert!(
            !acknowledges_non_linux_boundary(Some(std::ffi::OsStr::new(rejected))),
            "#448 only the exact value 1 may acknowledge the non-Linux boundary: {rejected:?}"
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn cargo_invocation_boundary() {
    if acknowledges_non_linux_boundary(
        std::env::var_os("PDRS_UNSAFE_BOUNDARY_ALLOW_NON_LINUX").as_deref(),
    ) {
        stderr_boundary_line(
            "#448 skip: the /proc invocation boundary is Linux-only; PDRS_UNSAFE_BOUNDARY_ALLOW_NON_LINUX=1 acknowledges the manual-review boundary (invocation directory chain, runner argv and CARGO identity are unverified on this platform)\n",
        );
        return;
    }
    panic!(
        "#448 the /proc invocation boundary is unavailable on this platform, so the invocation directory chain, the runner argv and the CARGO identity cannot be verified; refusing to report an unverified green. For this guard test only, set PDRS_UNSAFE_BOUNDARY_ALLOW_NON_LINUX=1 to acknowledge the manual-review boundary and skip with a printed line"
    );
}

#[test]
fn compiler_enforces_gateway_boundary() {
    let gateway = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = gateway.parent().unwrap();
    let root = workspace.parent().unwrap();
    compiler_environment_boundary();
    toolchain_environment_boundary(root);
    cargo_configuration_locations_boundary(workspace);
    cargo_invocation_boundary();
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

// Negative controls for the vendored source scanner. The guard's only positive
// control is the real vendored tree: passing there proves the current inputs
// work, not that a bad input goes red. Each case below feeds a throwaway
// vendored-shaped tree to `vendored_source_edges` /
// `collect_vendored_source_candidates` and pins the rejection reason, so a
// future scanner change cannot silently turn a rejection into a skip.
#[cfg(test)]
mod vendored_edge_negative_controls {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Fixture {
        root: PathBuf,
        package: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let root = std::env::temp_dir().join(format!(
                "pdrs-449-edges-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("pkg/src")).unwrap();
            // The edge check compares canonical targets with the package
            // directory, so make the fixture root canonical (macOS /var is a
            // symlink to /private/var).
            let root = root.canonicalize().unwrap();
            let package = root.join("pkg");
            Self { root, package }
        }

        fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }

        fn source(&self, relative: &str, text: &str) -> PathBuf {
            Self::write(&self.package.join("src"), relative, text)
        }

        fn outside(&self, relative: &str, text: &str) -> PathBuf {
            Self::write(&self.root, relative, text)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    // Every rejection is a panic; capture the message so a test asserts the
    // reason, not merely "something failed".
    fn rejection(action: impl FnOnce()) -> String {
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action))
            .expect_err("the guard must reject this source edge");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_owned())
            })
            .unwrap_or_default()
    }

    fn scan(package: &Path, source: &Path) -> Vec<PathBuf> {
        vendored_source_edges("fixture", package, source)
    }

    #[test]
    fn escaping_path_attribute_is_rejected() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let source = fixture.source("evil.rs", "#[path = \"../../outside.rs\"]\nmod m;\n");
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(
            message.contains("escapes its package directory"),
            "{message}"
        );
    }

    #[test]
    fn raw_path_attribute_is_rejected() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let source = fixture.source("evil.rs", "#[r#path = \"../../outside.rs\"]\nmod m;\n");
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(
            message.contains("escapes its package directory"),
            "{message}"
        );
    }

    #[test]
    fn cfg_attr_raw_path_is_rejected() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let source = fixture.source(
            "evil.rs",
            "#[cfg_attr(all(), r#path = \"../../outside.rs\")]\nmod m;\n",
        );
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(
            message.contains("escapes its package directory"),
            "{message}"
        );
    }

    #[test]
    fn inline_module_path_is_rejected_with_decoy_present() {
        let fixture = Fixture::new();
        // The decoy is what the old scanner resolved; the real rustc target
        // lives one directory deeper. Both exist so only the inline rule can
        // reject this source.
        fixture.source("b/leaf.rs", "// decoy\n");
        fixture.source("a/b/leaf.rs", "// rustc target\n");
        for attribute in [
            "#[path = \"b/leaf.rs\"]",
            "#[r#path = \"b/leaf.rs\"]",
            "#[cfg_attr(all(), path = \"b/leaf.rs\")]",
        ] {
            let source = fixture.source("evil.rs", &format!("mod a {{ {attribute} mod m; }}\n"));
            let message = rejection(|| {
                scan(&fixture.package, &source);
            });
            assert!(message.contains("file top level"), "{attribute}: {message}");
        }
        // Nested inline modules are rejected too; the depth must not pop early.
        let source = fixture.source(
            "nested.rs",
            "mod a { mod b { #[path = \"leaf.rs\"] mod m; } }\n",
        );
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(message.contains("file top level"), "{message}");
    }

    #[test]
    fn inline_module_without_path_is_allowed() {
        let fixture = Fixture::new();
        let source = fixture.source(
            "plain.rs",
            "mod a { pub fn f() {} mod b { pub fn g() {} } }\n",
        );
        assert!(scan(&fixture.package, &source).is_empty());
    }

    #[test]
    fn macro_expanded_inline_module_path_is_rejected_with_decoy_present() {
        let fixture = Fixture::new();
        // Coordinator's rustc repro: `gen!(inner)` expands to `mod inner { ... }`,
        // so rustc's base is `src/inner/`. The old scanner only recognized
        // `mod <ident> {`, treated this as a file-top-level `#[path]`, and
        // audited `src/leaf.inc` (the decoy).
        fixture.source("leaf.inc", "// decoy: what the old scanner audits\n");
        fixture.source("inner/leaf.inc", "// rustc target after gen!(inner)\n");
        let source = fixture.source(
            "evil.rs",
            "macro_rules! gen { ($i:ident) => { mod $i { #[path = \"leaf.inc\"] pub mod m; } } }\n\
             gen!(inner);\n",
        );
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(message.contains("file top level"), "{message}");
        assert!(message.contains("expansion context"), "{message}");
    }

    #[test]
    fn macro_argument_path_attribute_is_rejected() {
        let fixture = Fixture::new();
        fixture.source("x", "// decoy target for the old file-directory join\n");
        let source = fixture.source("evil.rs", "gen!( #[path = \"x\"] mod m; );\n");
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(message.contains("file top level"), "{message}");
    }

    #[test]
    fn function_body_module_path_is_rejected() {
        let fixture = Fixture::new();
        fixture.source("x", "// decoy target\n");
        let source = fixture.source("evil.rs", "fn f() { mod m { #[path = \"x\"] mod n; } }\n");
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(message.contains("file top level"), "{message}");
    }

    #[test]
    fn macro_definition_include_is_rejected() {
        let fixture = Fixture::new();
        fixture.source("data.inc", "// target that the old scanner resolves\n");
        for invocation in ["include!", "r#include!"] {
            let source = fixture.source(
                "evil.rs",
                &format!("macro_rules! gen {{ () => {{ {invocation}(\"data.inc\"); }} }}\n"),
            );
            let message = rejection(|| {
                scan(&fixture.package, &source);
            });
            assert!(message.contains("macro_rules!"), "{invocation}: {message}");
        }
    }

    #[test]
    fn non_rust_comment_obfuscated_edges_are_scanned() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let included_file = fixture.source(
            "data/obfuscated.inc",
            "include/*c*/!(\"../../../outside.rs\");\n",
        );
        let path = fixture.source(
            "data/obfuscated_path.inc",
            "#/*c*/[/*c*/path = \"../../../outside.rs\"] mod m;\n",
        );
        let mut files = Vec::new();
        collect_vendored_source_candidates("fixture", &fixture.package, &mut files);
        assert!(files.contains(&included_file), "{files:?}");
        assert!(files.contains(&path), "{files:?}");
        let message = rejection(|| {
            scan(&fixture.package, &included_file);
        });
        assert!(
            message.contains("escapes its package directory"),
            "{message}"
        );
        let message = rejection(|| {
            scan(&fixture.package, &path);
        });
        assert!(
            message.contains("escapes its package directory"),
            "{message}"
        );
    }

    #[test]
    fn escaping_include_is_rejected() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let source = fixture.source("evil.rs", "include!(\"../../outside.rs\");\n");
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(
            message.contains("escapes its package directory"),
            "{message}"
        );
    }

    #[test]
    fn non_literal_arguments_are_rejected() {
        let fixture = Fixture::new();
        let path = fixture.source(
            "path.rs",
            "#[path = concat!(\"..\", \"/outside.rs\")]\nmod m;\n",
        );
        let message = rejection(|| {
            scan(&fixture.package, &path);
        });
        assert!(
            message.contains("#[path] must be a plain string literal"),
            "{message}"
        );
        let included_file = fixture.source(
            "include.rs",
            "include!(concat!(\"..\", \"/outside.rs\"));\n",
        );
        let message = rejection(|| {
            scan(&fixture.package, &included_file);
        });
        assert!(
            message.contains("include! must be a plain string literal"),
            "{message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_are_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let real = fixture.source("real.rs", "// real file inside the package\n");
        symlink(&real, fixture.package.join("src/link.rs")).unwrap();
        let source = fixture.source("evil.rs", "#[path = \"link.rs\"]\nmod m;\n");
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(message.contains("is a symlink"), "{message}");
        let included_file = fixture.source("evil_include.rs", "include!(\"link.rs\");\n");
        let message = rejection(|| {
            scan(&fixture.package, &included_file);
        });
        assert!(message.contains("is a symlink"), "{message}");
    }

    #[test]
    fn non_rust_tokenizer_failure_is_fail_closed() {
        let fixture = Fixture::new();
        // The edge marker makes the non-`.rs` file a candidate; a lexer
        // failure must reject it instead of skipping the file.
        let source = fixture.source("data/broken.inc", "include!(\"x\");\n\"unclosed\n");
        let message = rejection(|| {
            let mut files = Vec::new();
            collect_vendored_source_candidates("fixture", &fixture.package, &mut files);
        });
        assert!(message.contains("cannot be lexed as Rust"), "{message}");
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(message.contains("not lexable Rust"), "{message}");
    }

    // Every text-fallback marker branch is paired with a lexer failure: the
    // unclosed `"` keeps proc-macro2 from ever producing a token tree, so only
    // `text_names_edge` can reject the file. Each marker is asserted to reach
    // that branch (`lex_source` errors) before the guard is asked to reject it.
    #[test]
    fn unlexable_non_rust_marker_branches_fail_closed() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        for marker in [
            "#[path = \"../../outside.rs\"] mod m;",
            "#[r#path = \"../../outside.rs\"] mod m;",
            "#![path = \"../../outside.rs\"]",
            "#![r#path = \"../../outside.rs\"]",
            "include!(\"../../outside.rs\");",
            "r#include!(\"../../outside.rs\");",
            "#[cfg_attr(all(), path = \"../../outside.rs\")] mod m;",
        ] {
            let source = format!("{marker}\n\"unclosed\n");
            assert!(
                lex_source(&source).is_err(),
                "{marker}: not a lexer failure"
            );
            fixture.source("data/broken.inc", &source);
            let message = rejection(|| {
                let mut files = Vec::new();
                collect_vendored_source_candidates("fixture", &fixture.package, &mut files);
            });
            assert!(
                message.contains("cannot be lexed as Rust"),
                "{marker}: {message}"
            );
        }
    }

    // A marker split by a block comment never appears in the raw compact text;
    // only the comment-free view catches it. This is the comment-obfuscated
    // branch of the union, combined with a lexer failure so the token path is
    // out of reach.
    #[test]
    fn unlexable_non_rust_comment_obfuscated_include_fails_closed() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let source = "include/*c*/!(\"../../outside.rs\");\n\"unclosed\n";
        assert!(lex_source(source).is_err(), "not a lexer failure");
        fixture.source("data/obfuscated.inc", source);
        let message = rejection(|| {
            let mut files = Vec::new();
            collect_vendored_source_candidates("fixture", &fixture.package, &mut files);
        });
        assert!(message.contains("cannot be lexed as Rust"), "{message}");
    }

    // A `//` inside a string truncates the comment-free view at the URL, so the
    // marker that follows on the same line only survives in the raw compact
    // text. This is the raw-text branch of the union.
    #[test]
    fn unlexable_non_rust_string_truncated_marker_fails_closed() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let source =
            "const U: &str = \"http://x\"; #[path = \"../../outside.rs\"] mod m;\n\"unclosed\n";
        assert!(lex_source(source).is_err(), "not a lexer failure");
        fixture.source("data/url.inc", source);
        let message = rejection(|| {
            let mut files = Vec::new();
            collect_vendored_source_candidates("fixture", &fixture.package, &mut files);
        });
        assert!(message.contains("cannot be lexed as Rust"), "{message}");
    }

    #[test]
    fn plain_text_without_edge_markers_is_not_collected() {
        let fixture = Fixture::new();
        // The real vendored tree has files like these: `Cargo.toml` has
        // `path = "src/lib.rs"`, `LICENSE` contains the word "include", and
        // both fail proc-macro2's Rust token parse. They are not Rust source
        // and must not be collected or rejected.
        fixture.source(
            "data/Cargo.toml",
            "[package]\nname = \"x\"\n[lib]\npath = \"src/lib.rs\"\n",
        );
        fixture.source(
            "data/LICENSE",
            "Permission is hereby granted to include this text \"without\nrestriction, including without limitation the rights\n",
        );
        fixture.source("data/README.md", "This README mentions include and path.\n");
        let mut files = Vec::new();
        collect_vendored_source_candidates("fixture", &fixture.package, &mut files);
        assert!(files.is_empty(), "{files:?}");
    }

    #[test]
    fn normal_edges_resolve_inside_the_package() {
        let fixture = Fixture::new();
        let leaf = fixture.source("nested/leaf.rs", "// leaf\n");
        let attr = fixture.source("attr.rs", "// attr\n");
        let data = fixture.source("data.inc", "// include\n");
        let source = fixture.source(
            "lib.rs",
            "mod plain { pub fn f() {} }\n\
             mod plain_nested { mod inner { pub fn g() {} } }\n\
             #[path = \"nested/leaf.rs\"] mod leaf;\n\
             #[cfg_attr(all(), path = \"attr.rs\")] mod attr;\n\
             include!(\"data.inc\");\n\
             r#include!(\"data.inc\");\n",
        );
        let mut targets = scan(&fixture.package, &source);
        let data = data.canonicalize().unwrap();
        let mut expected = vec![
            leaf.canonicalize().unwrap(),
            attr.canonicalize().unwrap(),
            data.clone(),
            data,
        ];
        targets.sort();
        expected.sort();
        assert_eq!(targets, expected);
    }

    #[test]
    fn non_rust_files_naming_edges_are_scanned() {
        let fixture = Fixture::new();
        let lib = fixture.source("lib.rs", "// lib\n");
        let marker = fixture.source("data/blob.inc", "include!(\"x\");\n");
        // Whitespace variants still name the same edge.
        let spaced = fixture.source("data/spaced.inc", "include ! (\"x\");\n");
        let raw = fixture.source("data/raw.inc", "#[r#path = \"x\"] mod m;\n");
        fixture.source("data/plain.txt", "not source\n");
        // Invalid UTF-8 is binary data, which rustc cannot compile as source.
        fs::write(
            fixture.package.join("src/data/binary.bin"),
            [0xff, 0xfe, b'#', b'[', b'p'],
        )
        .unwrap();
        let mut files = Vec::new();
        collect_vendored_source_candidates("fixture", &fixture.package, &mut files);
        let mut expected = vec![lib, marker, spaced, raw];
        files.sort();
        expected.sort();
        assert_eq!(files, expected);
    }

    // r5 P0 regression: `br"\"` / `cr"\"` are valid raw byte/C strings. A hand
    // lexer read the `b` / `c` as an identifier and then treated `\"` as an
    // escaped quote, swallowing the real `#[path]` / `include!` that follows.
    #[test]
    fn raw_byte_and_c_string_prefixes_do_not_hide_edges() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        for source in [
            r#"const A: &[u8] = br"\"; #[path = "../../outside.rs"] mod m; const B: &str = "z";"#,
            r#"const A: &str = cr"\"; #[path = "../../outside.rs"] mod m; const B: &str = "z";"#,
        ] {
            let file = fixture.source("raw_path.rs", source);
            let message = rejection(|| {
                scan(&fixture.package, &file);
            });
            assert!(
                message.contains("escapes its package directory"),
                "{source}: {message}"
            );
        }
        for source in [
            r#"const A: &[u8] = br"\"; include!("../../outside.rs"); const B: &str = "z";"#,
            r#"const A: &str = cr"\"; include!("../../outside.rs"); const B: &str = "z";"#,
        ] {
            let file = fixture.source("raw_include.rs", source);
            let message = rejection(|| {
                scan(&fixture.package, &file);
            });
            assert!(
                message.contains("escapes its package directory"),
                "{source}: {message}"
            );
        }
        // Positive pairing: the same raw-string prefix with an in-package
        // target must still resolve, not be skipped or rejected.
        let leaf = fixture.source("inside.rs", "// inside\n");
        let valid_path = fixture.source(
            "raw_valid_path.rs",
            r#"const A: &[u8] = br"\"; #[path = "inside.rs"] mod m;"#,
        );
        assert_eq!(
            scan(&fixture.package, &valid_path),
            vec![leaf.canonicalize().unwrap()]
        );
        let data = fixture.source("inside.inc", "// inside include\n");
        let valid_include = fixture.source(
            "raw_valid_include.rs",
            r#"const A: &str = cr"\"; include!("inside.inc");"#,
        );
        assert_eq!(
            scan(&fixture.package, &valid_include),
            vec![data.canonicalize().unwrap()]
        );
    }

    // proc-macro2 turns `///` / `//!` into `#[doc = "..."]`. The doc text is a
    // string literal, so a `#[path]` written inside documentation is not an
    // edge; this is the positive control for the doc-name rule.
    #[test]
    fn doc_comment_text_is_not_an_edge() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let source = fixture.source(
            "documented.rs",
            "//! #[path = \"../../outside.rs\"]\n\
             /// #[path = \"../../outside.rs\"]\n\
             #[doc = \"#[path = \\\"../../outside.rs\\\"]\"]\n\
             fn f() {}\n",
        );
        assert!(scan(&fixture.package, &source).is_empty());
    }

    // Only a plain string without escapes or a raw string is a static path.
    // Bytes, C strings, escaped strings and suffixed literals must fail closed
    // instead of being read as a path with a prefix/suffix.
    #[test]
    fn non_plain_path_and_include_literals_are_rejected() {
        let fixture = Fixture::new();
        for literal in [
            r#"b"../outside.rs""#,
            r#"br"../outside.rs""#,
            r#"c"../outside.rs""#,
            r#"cr"../outside.rs""#,
            r#""..\u{2f}outside.rs""#,
            r#""x"suffix"#,
        ] {
            let source =
                fixture.source("literal_path.rs", &format!("#[path = {literal}] mod m;\n"));
            let message = rejection(|| {
                scan(&fixture.package, &source);
            });
            assert!(
                message.contains("#[path] must be a plain string literal"),
                "{literal}: {message}"
            );
        }
        for literal in [
            r#"b"data.inc""#,
            r#"br"data.inc""#,
            r#"c"data.inc""#,
            r#"cr"data.inc""#,
            r#""data\u{2e}inc""#,
            r#""x"suffix"#,
        ] {
            let source = fixture.source("literal_include.rs", &format!("include!({literal});\n"));
            let message = rejection(|| {
                scan(&fixture.package, &source);
            });
            assert!(
                message.contains("include! must be a plain string literal"),
                "{literal}: {message}"
            );
        }
    }

    #[test]
    fn raw_string_path_and_include_are_accepted() {
        let fixture = Fixture::new();
        let raw_path = fixture.source("raw/leaf.rs", "// raw path target\n");
        let raw_include = fixture.source("raw/data.inc", "// raw include target\n");
        let source = fixture.source(
            "raw_literals.rs",
            "#[path = r\"raw/leaf.rs\"] mod leaf;\n\
             include!(r#\"raw/data.inc\"#);\n",
        );
        let mut targets = scan(&fixture.package, &source);
        let mut expected = vec![
            raw_path.canonicalize().unwrap(),
            raw_include.canonicalize().unwrap(),
        ];
        targets.sort();
        expected.sort();
        assert_eq!(targets, expected);
    }

    #[test]
    fn unlexable_rust_source_is_fail_closed() {
        let fixture = Fixture::new();
        // `.rs` files are always candidates; a lexer failure must reject them
        // instead of skipping or falling back to text.
        let source = fixture.source("broken.rs", "\"unclosed\n");
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(message.contains("not lexable Rust"), "{message}");
    }

    #[test]
    fn non_utf8_rust_source_is_fail_closed() {
        let fixture = Fixture::new();
        let source = fixture.package.join("src/broken.rs");
        fs::write(&source, [0xff, 0xfe, b'f', b'n']).unwrap();
        let message = rejection(|| {
            scan(&fixture.package, &source);
        });
        assert!(message.contains("not valid UTF-8"), "{message}");
    }

    fn vendored_metadata(targets: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": "fixture",
            "dependencies": [],
            "targets": targets,
        })
    }

    fn vendored_boundary(package: &Path, metadata: &serde_json::Value) {
        vendored_package_boundary("fixture", metadata, package, &BTreeSet::new());
    }

    #[test]
    fn vendored_non_lib_targets_are_rejected() {
        let fixture = Fixture::new();
        let lib = fixture.source("lib.rs", "// lib\n");
        let metadata = vendored_metadata(serde_json::json!([
            {"kind": ["bin"], "src_path": lib}
        ]));
        let message = rejection(|| {
            vendored_boundary(&fixture.package, &metadata);
        });
        assert!(
            message.contains("must expose only a lib target"),
            "{message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn vendored_symlinked_lib_source_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let real = fixture.source("real_lib.rs", "// lib\n");
        let link = fixture.package.join("src/link_lib.rs");
        symlink(&real, &link).unwrap();
        let metadata = vendored_metadata(serde_json::json!([
            {"kind": ["lib"], "src_path": link}
        ]));
        let message = rejection(|| {
            vendored_boundary(&fixture.package, &metadata);
        });
        assert!(message.contains("lib source is a symlink"), "{message}");
    }

    #[test]
    fn vendored_lib_source_escape_is_rejected() {
        let fixture = Fixture::new();
        let outside = fixture.outside("outside.rs", "// outside the package\n");
        let metadata = vendored_metadata(serde_json::json!([
            {"kind": ["lib"], "src_path": outside}
        ]));
        let message = rejection(|| {
            vendored_boundary(&fixture.package, &metadata);
        });
        assert!(
            message.contains("lib source escapes its package directory"),
            "{message}"
        );
    }

    #[test]
    fn vendored_lib_source_that_is_not_a_file_is_rejected() {
        let fixture = Fixture::new();
        let directory = fixture.package.join("src");
        let metadata = vendored_metadata(serde_json::json!([
            {"kind": ["lib"], "src_path": directory}
        ]));
        let message = rejection(|| {
            vendored_boundary(&fixture.package, &metadata);
        });
        assert!(message.contains("lib source is not a file"), "{message}");
    }

    #[test]
    fn vendored_source_target_not_a_file_and_missing_are_rejected() {
        let fixture = Fixture::new();
        let source = fixture.source("lib.rs", "// lib\n");
        let message = rejection(|| {
            assert_vendored_source_target("fixture", &fixture.package, &source, "#[path]", ".");
        });
        assert!(message.contains("target is not a file"), "{message}");
        let message = rejection(|| {
            assert_vendored_source_target(
                "fixture",
                &fixture.package,
                &source,
                "#[path]",
                "missing.rs",
            );
        });
        assert!(message.contains("target cannot be resolved"), "{message}");
    }

    // The real vendored tree is the only positive control. This pins the
    // candidate sweep's no-false-positive property: `.rs` files are always
    // collected, but a plain text file that merely mentions `path` /
    // `include` (Cargo.toml, LICENSE, README, JSON/BPE data) must not be.
    #[test]
    fn real_vendored_candidates_are_rust_sources_only() {
        let gateway = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = gateway.parent().unwrap();
        let mut files = Vec::new();
        for package in VENDORED_PACKAGES {
            collect_vendored_source_candidates(package, &workspace.join(package), &mut files);
        }
        let mut non_rust: Vec<_> = files
            .iter()
            .filter(|file| file.extension().and_then(|extension| extension.to_str()) != Some("rs"))
            .collect();
        non_rust.sort();
        println!(
            "real vendored candidates: {} files, {} non-.rs",
            files.len(),
            non_rust.len()
        );
        let mut all: Vec<_> = files.iter().collect();
        all.sort();
        for file in &all {
            println!("candidate: {}", file.display());
        }
        for file in &non_rust {
            println!("non-.rs candidate: {}", file.display());
        }
        assert!(
            non_rust.is_empty(),
            "plain-text vendored files must not be candidates: {non_rust:?}"
        );
    }

    #[test]
    fn child_source_pin_rejects_a_changed_numeric_literal() {
        // The old byte lexer exposed numeric digits as tokens; the projection
        // keeps numeric literal text for the same reason. A changed signal
        // range must still break the full-file pin.
        let changed = CHILD_SOURCE.replacen("while signal < 32", "while signal < 31", 1);
        assert_ne!(changed, CHILD_SOURCE);
        let message = rejection(|| {
            child_source_boundary(&changed);
        });
        assert!(
            message.contains("pilotdeck-child-signals/src/lib.rs changed"),
            "{message}"
        );
    }

    #[test]
    fn vendored_compile_closure_follows_transitive_includes() {
        let fixture = Fixture::new();
        fixture.outside("outside.rs", "// outside the package\n");
        let lib = fixture.source("lib.rs", "include!(\"a.inc\");\n");
        fixture.source("a.inc", "include!(\"b.inc\");\n");
        fixture.source("b.inc", "#[path = \"../../outside.rs\"] mod m;\n");
        let metadata = vendored_metadata(serde_json::json!([
            {"kind": ["lib"], "src_path": lib}
        ]));
        let message = rejection(|| {
            vendored_boundary(&fixture.package, &metadata);
        });
        assert!(
            message.contains("#[path] target escapes its package directory"),
            "{message}"
        );
    }
}
