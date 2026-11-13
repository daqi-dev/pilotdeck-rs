//! #2558: the A4 gateway's error console writes never panic when stderr
//! fails. The #2558 sites — the A4 entry (`main.rs` startup/accept/
//! connection), `http_proxy.rs`'s four error lines, `upstream.rs`'s
//! connection task, the memory maintenance default diagnostic, the native
//! entry's listening/stop/halt lines (`src/native_process.rs`, fix-r1),
//! and the default warn sinks (`refresh.rs`, `replacement.rs`,
//! `plugins.rs`, fix-r1) — go through the shared `console_log_line`, which
//! drops the write error like TS's `console.error`, while `println!`/
//! `eprintln!` panic on a failed write (std behavior). `native_ws.rs`
//! joined the same shape in #2434. fix-r2 closed the same face on the
//! native assembly's own startup witnesses, acquire-miss report, and
//! default session diagnostics (`native_process/assembly.rs`), the default
//! tail debug sink (`runtime/turn/runner_tail.rs`), the request-side
//! best-effort lines (`runtime/turn/submit.rs`,
//! `runtime/project/interaction_notice.rs`), the status/persist/abort/
//! registration lines (`channel/runtime_status.rs`,
//! `channel/weixin/state.rs`, `switch.rs`, `lib.rs`), the model dump
//! lines (`runtime/model/http.rs`, its stdout receipt included), and
//! `refresh.rs`'s stdout `log` default. fix-r3 added the TLS CA warn
//! lines (`runtime/model/http/tls.rs`, on `client_config()`'s
//! `OnceLock` path) and, with them, the tree scan
//! (`production_tree_stays_eprintln_free`) that polices the whole
//! production tree instead of only the listed files — tls.rs never named
//! `console_log_line`, so no derived rule had pulled it into either
//! table while it still carried eight `eprintln!` sites. fix-r4 closed
//! the stdout face of the same scans: the entry's banner `println!`s
//! (`main.rs`, the two lines whose stderr neighbors were already fixed)
//! went through the writer, the detector's match set widened from
//! `eprintln` alone to all three panicking console macros (`eprintln` /
//! `println` / `print`), and the test-support `println!` diagnostics the
//! widened scan newly reached — gated where the in-file `#[cfg(test)]`
//! skip cannot see it (the `host/tests/` declaration site, the
//! `#[cfg(all(test, unix))]` spelling) — were converted to the writer
//! too, so the widened scan stays green without widening any exemption.
//! The lib-side probes (`http_proxy::tests`,
//! `upstream::tests`, `runtime::memory::maintenance::tests`, plus
//! `native_process::channels_tests`' failing-sink unit case) drive the
//! reachable sites in re-exec'd children with EPIPE stderr; this file owns
//! what only the real binary can pin — the entry's own three sites — plus
//! the lexical guards that keep every #2558 file free of the panicking
//! console macros (`eprintln!` / `println!` / `print!`, fix-r4) for the
//! branches no portable child can reach (the accept-error line has no
//! deterministic fd-exhaustion trigger, and a panic inside the entry's
//! spawned connection task is contained by tokio, invisible to this
//! process).

#[allow(dead_code)]
#[path = "common/source_inputs.rs"]
mod source_inputs;

use source_inputs::SourceToken;
use std::{
    fs,
    io::{BufRead, BufReader, Read as _, Write as _},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::Duration,
};

/// The files that own the #2558 error lines plus the shared writer itself
/// (its definition in `native_process/channels.rs`, the native entry that
/// re-exports it, and `native_ws.rs`, which routed its accept-error lines
/// through the writer in #2434; fix-r2 added the assembly's own
/// diagnostics, the default tail/debug/log sinks, and the status/dump/
/// abort/registration lines listed in the module docs; fix-r3 added the
/// TLS CA warn lines in `runtime/model/http/tls.rs`; fix-r4 added the
/// three test-support files whose `println!` diagnostics the widened
/// three-macro scan reaches although their gating — the `host/tests.rs`
/// declaration site, the `#[cfg(all(test, unix))]` spelling in
/// `file_memory.rs` — is invisible to the in-file `#[cfg(test)]` skip:
/// converted to the writer rather than widening any exemption). Kept
/// complete against new callers by `shared_writer_call_sites_stay_listed`,
/// and since fix-r3 every entry is also an anchor of the whole-tree scan in
/// `production_tree_stays_eprintln_free`. Scanned through the shared
/// proc-macro2 lexer (`common/source_inputs`), never a text search:
/// comments and string literals — including the doc comments that quote
/// the macro — are token kinds there and cannot false-positive.
const EPRINTLN_FREE_SOURCES: [&str; 23] = [
    "src/main.rs",
    "src/http_proxy.rs",
    "src/upstream.rs",
    "src/runtime/memory/maintenance.rs",
    "src/native_process/channels.rs",
    "src/native_process.rs",
    "src/native_ws.rs",
    "src/runtime/refresh.rs",
    "src/runtime/replacement.rs",
    "src/runtime/plugins.rs",
    "src/native_process/assembly.rs",
    "src/runtime/turn/runner_tail.rs",
    "src/runtime/turn/submit.rs",
    "src/runtime/project/interaction_notice.rs",
    "src/channel/runtime_status.rs",
    "src/channel/weixin/state.rs",
    "src/switch.rs",
    "src/runtime/model/http.rs",
    "src/lib.rs",
    "src/runtime/model/http/tls.rs",
    "src/host/tests/resume.rs",
    "src/host/tests/cancel_exit.rs",
    "src/runtime/memory/edgeclaw/file_memory.rs",
];

/// Lexical guard: none of the #2558 files may write an error console line
/// with a panicking console macro (`eprintln!`, `println!`, `print!`;
/// fix-r4 widened the set from `eprintln!` alone — the entry's stdout
/// banners had stayed bare on the very file whose stderr lines were
/// already fixed) again. This is the only killable pin for
/// the two entry lines no child can observe (the accept-error branch has no
/// portable trigger, and the spawned connection task's panic is contained
/// by tokio), and it kills every site mutation even where the behavioral
/// probes already do. Items gated by `#[cfg(test)]` are skipped (fix-r2):
/// their subtree is libtest-support code that the production build removes
/// (`assembly.rs`'s embedded-entry `start` keeps one), so a listed file
/// can carry test-support diagnostics without failing the product scan.
#[test]
fn error_console_writes_stay_non_panicking() {
    for relative in EPRINTLN_FREE_SOURCES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("#2558 reading {relative}: {error}"));
        let tokens = source_inputs::lex_source(&source)
            .unwrap_or_else(|error| panic!("#2558 lexing {relative}: {error}"));
        // A lexed-to-empty file (truncated or emptied) would pass the scan
        // vacuously; the scan is only evidence with tokens to scan.
        assert!(
            !tokens.is_empty(),
            "#2558 {relative} lexed to an empty token stream; the guard would \
             pass without scanning anything"
        );
        assert_no_eprintln(&tokens, relative);
    }
}

/// The guard must be able to fail (DS r1 boundary#1): an `eprintln!` site
/// fed to `assert_no_eprintln` has to panic, so an emptied or silently
/// weakened detector cannot pass as a green run of the file scan above.
/// fix-r4 feeds all three console macros — the widened match set is
/// load-bearing per macro, not only on the stderr spelling it started
/// from, so a detector reverted to `eprintln`-alone fails here too.
#[test]
fn assert_no_eprintln_kills_a_real_eprintln() {
    for word in ["eprintln", "println", "print"] {
        let fixture = format!("fn f() {{ {word}!(\"#2558 fixture line\"); }}");
        let tokens = source_inputs::lex_source(&fixture)
            .unwrap_or_else(|error| panic!("#2558 lexing the self-test fixture: {error}"));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_no_eprintln(&tokens, "fixture.rs")
        }))
        .expect_err(&format!(
            "#2558 the detector must panic on a real {word}! site"
        ));
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            message.contains("#2558 fixture.rs still writes an error console line"),
            "#2558 the detector's panic must name the offending file, got {message:?}"
        );
    }
}

/// The cfg(test) exemption (fix-r2) must hold in both directions: a gated
/// `eprintln!` is libtest-support code and does not fail the scan, while a
/// production site after a gated item is still detected — an over-eager
/// skip (say, one that resumes only at end-of-file) would pass the first
/// half by swallowing the second.
#[test]
fn assert_no_eprintln_skips_only_cfg_test_items() {
    // Gated spellings that must not fail the scan: the `mod tests` block, a
    // gated `fn` whose generic return type carries a flat `,` (the
    // `assembly.rs` shape this exemption exists for), and a gated struct
    // field with productions behind it.
    for gated in [
        concat!(
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    fn helper() { eprintln!(\"#2558 test-only line\"); }\n",
            "}\n",
        ),
        concat!(
            "#[cfg(test)]\n",
            "pub async fn start() -> Result<u8, u16> { eprintln!(\"#2558 test-only line\"); Ok(0) }\n",
        ),
        concat!(
            "struct Hooks {\n",
            "    shared: u8,\n",
            "    #[cfg(test)]\n",
            "    hooks: u8,\n",
            "    later: u8,\n",
            "}\n",
        ),
    ] {
        let tokens = source_inputs::lex_source(gated)
            .unwrap_or_else(|error| panic!("#2558 lexing the gated fixture: {error}"));
        assert_no_eprintln(&tokens, "fixture.rs");
    }
    // Production sites that must still fail the scan: right after a gated
    // item, and in the production arm behind a gated match arm (the `,`
    // boundary the generic-comma rule must not swallow).
    for adjacent in [
        concat!(
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    fn helper() { eprintln!(\"#2558 test-only line\"); }\n",
            "}\n",
            "fn prod() { eprintln!(\"#2558 production line\"); }\n",
        ),
        concat!(
            "fn prod(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        0 => helper(),\n",
            "        _ => eprintln!(\"#2558 production line\"),\n",
            "    }\n",
            "}\n",
        ),
    ] {
        let tokens = source_inputs::lex_source(adjacent)
            .unwrap_or_else(|error| panic!("#2558 lexing the adjacent fixture: {error}"));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_no_eprintln(&tokens, "fixture.rs")
        }))
        .expect_err("#2558 a production site must still fail the scan");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            message.contains("#2558 fixture.rs still writes an error console line"),
            "#2558 the detector's panic must name the offending file, got {message:?}"
        );
    }
}

/// Every production source file that lexically names the shared writer must
/// be listed in `EPRINTLN_FREE_SOURCES` (fix-r1 review MM sibling#4's
/// derived rule): the guard is the only killable pin for sites without a
/// portable EPIPE trigger, so an unlisted caller would silently escape it
/// — exactly how `native_ws.rs` (a #2434 caller) sat outside the table
/// until fix-r1. Naming the writer counts a call, the definition, or a
/// re-export; comments and doc text quote it only as non-`Ident` tokens.
/// Test-support files (the crate's three spellings, see
/// [`is_test_support_source`]) are excluded: their diagnostics are
/// libtest-support code, not product console lines.
#[test]
fn shared_writer_call_sites_stay_listed() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut derived = Vec::new();
    for relative in production_rust_sources(manifest) {
        let bytes = fs::read(manifest.join(&relative))
            .unwrap_or_else(|error| panic!("#2558 reading {relative}: {error}"));
        let Ok(source) = String::from_utf8(bytes) else {
            // A non-UTF-8 payload is not Rust source (rustc cannot
            // compile it either); it cannot name the writer.
            continue;
        };
        let tokens = source_inputs::lex_source(&source).unwrap_or_else(|error| {
            panic!("#2558 {relative} is not lexable Rust; fail closed: {error}")
        });
        if tokens_contain_ident(&tokens, "console_log_line") {
            derived.push(relative);
        }
    }
    // The walk itself must find callers: a silently broken walk (wrong
    // root, wrong exclusion) would pass vacuously, so anchor it on a
    // caller every shape of this crate has had since #2558 r0 — the A4
    // entry's own file.
    assert!(
        derived.iter().any(|caller| caller == "src/main.rs"),
        "#2558 the walk found no caller in src/main.rs; the derived set is \
         wrong: {derived:?}"
    );
    for caller in &derived {
        assert!(
            EPRINTLN_FREE_SOURCES.contains(&caller.as_str()),
            "#2558 {caller} names the shared writer but is not in \
             EPRINTLN_FREE_SOURCES; the eprintln-free guard never scans it"
        );
    }
}

/// fix-r3 (review P1): the whole production tree, not only the listed
/// files, stays `eprintln!`-free. `runtime/model/http/tls.rs` sat outside
/// both tables while carrying eight production `eprintln!` sites on
/// `client_config()`'s `OnceLock` path: it never named `console_log_line`,
/// so the derived listing rule never pulled it in, and nobody had added it
/// to `EPRINTLN_FREE_SOURCES` — both guards were green while the crate
/// still panicked on a failed stderr write. This scan closes that escape
/// class: any production file under `src/` that writes the panicking
/// `eprintln!` fails, listed or not. fix-r4 widened the detector's match
/// set to all three panicking console macros, so the same scan now polices
/// the stdout face too — and the widening immediately reached three
/// test-support files whose gating is invisible to the in-file skip
/// (`host/tests/resume.rs` / `host/tests/cancel_exit.rs`, gated at the
/// `host/tests.rs` declaration; `file_memory.rs`'s
/// `#[cfg(all(test, unix))]` modules): their `println!` diagnostics were
/// converted to the writer, the fail-closed remedy that widens no
/// exemption. Items gated by `#[cfg(test)]` inside
/// a scanned file are skipped by the same detector the listed-file scan
/// uses; test-support file spellings are excluded by
/// [`production_rust_sources`]. An unhandled test-gating spelling (a
/// top-level `#[test] fn`, a `#[cfg(all(test, ...))]` module file with a
/// production name) fails the scan rather than passing silently — extend
/// the exclusion consciously then.
#[test]
fn production_tree_stays_eprintln_free() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = production_rust_sources(manifest);
    // A silently broken walk (wrong root, no descent into subdirectories,
    // an over-wide exclusion) would pass the scan vacuously; every listed
    // #2558 file must be reached.
    for listed in EPRINTLN_FREE_SOURCES {
        assert!(
            sources.iter().any(|source| source == listed),
            "#2558 the production walk skipped the #2558 source {listed}; \
             the tree scan would pass without scanning it: found {sources:?}"
        );
    }
    for relative in &sources {
        let bytes = fs::read(manifest.join(relative))
            .unwrap_or_else(|error| panic!("#2558 reading {relative}: {error}"));
        let Ok(source) = String::from_utf8(bytes) else {
            // A non-UTF-8 payload is not Rust source (rustc cannot
            // compile it either); it cannot hold a production line.
            continue;
        };
        let tokens = source_inputs::lex_source(&source)
            .unwrap_or_else(|error| panic!("#2558 lexing {relative}: {error}"));
        // A lexed-to-empty file (truncated or emptied) would pass the scan
        // vacuously; the scan is only evidence with tokens to scan.
        assert!(
            !tokens.is_empty(),
            "#2558 {relative} lexed to an empty token stream; the guard would \
             pass without scanning anything"
        );
        assert_no_eprintln(&tokens, relative);
    }
}

/// The crate's three src-side test-support file spellings: the inline
/// `mod tests` targets (`tests.rs`, `*_tests.rs`) and the `test_*` family
/// that `lib.rs` declares under `#[cfg(test)]` (`test_process`,
/// `test_scratch`, `test_support`, ... — gated at the declaration, so the
/// files themselves carry no `#[cfg(test)]` items to skip). Every src/
/// module that is test-only by declaration today is one of these names;
/// a new spelling is not exempt, and its diagnostics fail the production
/// scan until this predicate grows (fail-closed).
fn is_test_support_source(name: &str) -> bool {
    name == "tests.rs" || name.ends_with("_tests.rs") || name.starts_with("test_")
}

/// Every production Rust source under `src/`, as manifest-relative
/// slash-separated paths, in the lexical guards' vocabulary: all `.rs`
/// files except the test-support spellings
/// ([`is_test_support_source`]). Deterministic order (each directory's
/// entries sorted) so a failure names the same file on every run.
fn production_rust_sources(manifest: &Path) -> Vec<String> {
    let mut sources = Vec::new();
    let mut stack = vec![manifest.join("src")];
    while let Some(directory) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&directory)
            .unwrap_or_else(|error| {
                panic!(
                    "#2558 reading {}: {error}",
                    directory
                        .strip_prefix(manifest)
                        .unwrap_or(&directory)
                        .display()
                )
            })
            .map(|entry| entry.expect("#2558 a directory entry"))
            .collect();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if entry.file_type().expect("#2558 an entry type").is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_test_support_source)
            {
                continue;
            }
            sources.push(
                path.strip_prefix(manifest)
                    .expect("#2558 source sits under the manifest")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    sources
}

fn tokens_contain_ident(tokens: &[SourceToken], word: &str) -> bool {
    tokens.iter().any(|token| match token {
        SourceToken::Ident(name) => name == word,
        SourceToken::Group { tokens, .. } => tokens_contain_ident(tokens, word),
        _ => false,
    })
}

fn assert_no_eprintln(tokens: &[SourceToken], file: &str) {
    let mut index = 0;
    while index < tokens.len() {
        match &tokens[index] {
            // fix-r4: all three panicking console macros, not `eprintln`
            // alone — the stdout face (a bare `println!` banner) panics on
            // a failed write exactly like the stderr spelling, so the
            // match set grew to cover it; only the match arm changed, the
            // exemption predicates and the `#[cfg(test)]` skip did not.
            SourceToken::Ident(word)
                if word == "eprintln" || word == "println" || word == "print" =>
            {
                assert!(
                    tokens.get(index + 1) != Some(&SourceToken::Punct('!')),
                    "#2558 {file} still writes an error console line with the \
                     panicking `{word}!` (std panics when the write fails); \
                     route it through `console_log_line` instead"
                );
                index += 1;
            }
            SourceToken::Punct('#') if is_cfg_test_attribute(tokens.get(index + 1)) => {
                index = index_past_cfg_test_item(tokens, index);
            }
            SourceToken::Group { tokens, .. } => {
                assert_no_eprintln(tokens, file);
                index += 1;
            }
            _ => index += 1,
        }
    }
}

/// The `#[cfg(test)]` attribute shape: a `#` followed by the bracket group
/// holding exactly `cfg ( test )`. Other predicates — `cfg(all(test, ...))`,
/// `cfg_attr`, — do not match, so the exemption cannot silently widen: a
/// test-support `eprintln!` under any other predicate still fails the scan
/// (fail-closed), and only the one predicate rustc actually keys libtest
/// support on is skipped.
fn is_cfg_test_attribute(token: Option<&SourceToken>) -> bool {
    let Some(SourceToken::Group {
        delimiter: proc_macro2::Delimiter::Bracket,
        tokens: predicate,
    }) = token
    else {
        return false;
    };
    match predicate.as_slice() {
        [
            SourceToken::Ident(name),
            SourceToken::Group {
                delimiter: proc_macro2::Delimiter::Parenthesis,
                tokens: arguments,
            },
        ] if name.as_str() == "cfg" => {
            matches!(arguments.as_slice(), [SourceToken::Ident(subject)] if subject == "test")
        }
        _ => false,
    }
}

/// Resume the scan past the item a `#[cfg(test)]` attribute gates. The
/// gated item is test-only code — the production build compiles it out —
/// so its subtree cannot hold a production error line. The item ends at
/// its body (the first brace group: `fn` / `mod` / `impl` / block
/// spellings) or at its own terminator — `;` (`use` / `mod;` / `let`
/// spellings) or `,` (field / match-arm / element spellings) — whichever
/// comes first, so the skip never swallows the productions that follow
/// it: a `#[cfg(test)]` match arm stops at its `,` and the production arms
/// behind it stay scanned. A `,` only counts at the item's own level: the
/// commas inside a generic list (`Result<TurnGateway, ProxyError>`, the
/// `assembly.rs` shape this exists for) sit between an unclosed `<` … `>`
/// pair and are not delimiters, so they are tracked and skipped. The angle
/// count is heuristic where expressions meet comparisons (`a <= b` opens
/// a phantom pair) — a miscount can only miss a `,` terminator, which
/// skips too far inside one `,`-list, never past a body or `;`.
fn index_past_cfg_test_item(tokens: &[SourceToken], attribute: usize) -> usize {
    let mut index = attribute;
    let mut angle_depth = 0usize;
    while index < tokens.len() {
        match &tokens[index] {
            SourceToken::Group {
                delimiter: proc_macro2::Delimiter::Brace,
                ..
            }
            | SourceToken::Punct(';') => return index + 1,
            SourceToken::Punct(',') if angle_depth == 0 => return index + 1,
            SourceToken::Punct('<') => angle_depth += 1,
            SourceToken::Punct('>') => angle_depth = angle_depth.saturating_sub(1),
            _ => {}
        }
        index += 1;
    }
    index
}

fn isolated_home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("pdrs-2558-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&home).expect("#2558 create isolated home");
    home
}

struct Gateway {
    child: Child,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    home: PathBuf,
}

impl Gateway {
    /// Start the real A4 proxy entry (the `pilotdeck-gateway` binary) on an
    /// ephemeral loopback port with a dead upstream, like the
    /// `proxy_listen_address` precedent. `stderr` selects the probe's sink:
    /// a capture file, or a pipe whose read end
    /// [`Self::fail_stderr`] then drops.
    fn start(home: &Path, stderr: Stdio) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
            .env_clear()
            .env("HOME", home)
            .env("PILOT_HOME", home.join("pilot"))
            .env("PILOTDECK_PROXY_LISTEN", "127.0.0.1:0")
            .env("PILOTDECK_PROXY_UPSTREAM", "http://127.0.0.1:1")
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .expect("#2558 spawn real gateway binary");
        let stdout = child.stdout.take().expect("#2558 capture gateway stdout");
        let (send, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                if reader
                    .read_line(&mut line)
                    .expect("#2558 read gateway stdout")
                    == 0
                {
                    break;
                }
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
            home: home.to_path_buf(),
        }
    }

    /// The failing sink: a pipe whose read end is dropped before the child
    /// writes its startup diagnostic, so every stderr write fails with
    /// EPIPE. (A read-only fd is not a failing sink for this purpose:
    /// `std::io::Stderr` redirects EBADF to a fresh `/dev/null` and reports
    /// success — only EPIPE reaches an `eprintln!` panic.)
    fn fail_stderr(&mut self) {
        drop(self.child.stderr.take());
    }

    fn line(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(15))
            .unwrap_or_else(|error| {
                panic!(
                    "#2558 gateway must publish address on stdout: {error}; stderr: {}",
                    fs::read_to_string(self.home.join("stderr.log"))
                        .expect("#2558 read startup diagnostics")
                )
            })
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn listening_address(line: &str) -> SocketAddr {
    line.strip_prefix("PilotDeck server listening: http://")
        .unwrap_or_else(|| panic!("#2558 the HTTP banner line: {line:?}"))
        .strip_suffix('\n')
        .expect("#2558 banner line ends with newline")
        .parse()
        .expect("#2558 banner address parses as a socket address")
}

/// #2558: the startup metadata line stays a stderr diagnostic — written
/// right after the two banner lines, before the accept loop — and the write
/// itself can no longer kill the entry (the order is not panic-dependent:
/// the `current_dir()` error still propagates, only the console write is
/// best-effort). The bind precedes the banners, so a TCP connect alone
/// cannot order the write; the entry emits it immediately after the
/// banners, and this side polls the capture for it inside a bounded window.
#[test]
fn startup_metadata_diagnostic_stays_on_stderr() {
    let home = isolated_home();
    let stderr_path = home.join("stderr.log");
    let stderr = fs::File::create(&stderr_path).expect("#2558 create stderr capture");
    let gateway = Gateway::start(&home, Stdio::from(stderr));
    let address = listening_address(&gateway.line());
    let ws = gateway.line();
    assert!(
        ws.starts_with("WebSocket: ws://"),
        "#2558 the WS banner line survives the diagnostic: {ws:?}"
    );
    TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .expect("#2558 the listener accepts");
    // `std::env::current_dir()` resolves symlinks (macOS temp dirs sit
    // behind /var -> /private/var), so compare the canonical form.
    let cwd = home
        .canonicalize()
        .unwrap_or_else(|_| home.clone())
        .display()
        .to_string();
    let expected = format!("metadata cwd: {cwd}");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let stderr_text = loop {
        let text = fs::read_to_string(&stderr_path).expect("#2558 read captured stderr");
        if text.contains(&expected) || std::time::Instant::now() >= deadline {
            break text;
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert!(
        stderr_text.contains(&expected),
        "#2558 the startup diagnostic stays on stderr naming the cwd, got {stderr_text:?}"
    );
}

/// #2558: the same entry with a failed stderr. The startup line was an
/// `eprintln!`; a failed write panicked inside the main task, killing the
/// process right after the banners — the accept loop that follows never
/// ran, and no HTTP request was ever answered. The non-panicking writer
/// keeps the entry alive. The receipt is a served 502: this side asks the
/// dead upstream (`127.0.0.1:1`) for a plain `GET /`, and only the accept
/// loop — the code after the diagnostic write — can answer it. (A TCP
/// connect alone would not do: the bind precedes the banners, so the kernel
/// accepts connections even from a process that panicked at the line in
/// between.)
#[test]
fn startup_survives_a_failed_stderr_write() {
    let home = isolated_home();
    let mut gateway = Gateway::start(&home, Stdio::piped());
    gateway.fail_stderr();
    let address = listening_address(&gateway.line());
    let ws = gateway.line();
    assert!(
        ws.starts_with("WebSocket: ws://"),
        "#2558 the WS banner line survives the failed write: {ws:?}"
    );
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .expect("#2558 the gateway keeps its listener after the failed write");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("#2558 the probe stream gets a read timeout");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .expect("#2558 the probe request is sent");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("#2558 the gateway's answer is readable");
    assert!(
        response.starts_with("HTTP/1.1 502"),
        "#2558 the entry serves HTTP past its failed stderr write, got {response:?}"
    );
}
