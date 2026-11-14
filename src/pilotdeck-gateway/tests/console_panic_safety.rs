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
//! `eprintln` alone to the panicking console macros, and the
//! test-support `println!` diagnostics the widened scan newly reached —
//! gated where the in-file `#[cfg(test)]` skip cannot see it (the
//! `host/tests/` declaration site, the `#[cfg(all(test, unix))]`
//! spelling) — were converted to the writer too, so the widened scan
//! stays green without widening any exemption. fix-r5 completed the
//! match set with `eprint`, the fourth and last panicking console macro:
//! std's `print!` / `println!` / `eprint!` / `eprintln!` all panic on a
//! failed write, and the newline-less stderr twin is exactly the face
//! #2558 started from, so leaving it out re-opened the r3 hole shape
//! (a green guard beside a panicking stderr write).
//! The lib-side probes (`http_proxy::tests`,
//! `upstream::tests`, `runtime::memory::maintenance::tests`, plus
//! `native_process::channels_tests`' failing-sink unit case) drive the
//! reachable sites in re-exec'd children with EPIPE stderr; this file owns
//! what only the real binary can pin — the entry's own three sites — plus
//! the lexical guards that keep every #2558 file free of the panicking
//! console macros (`eprintln!` / `println!` / `print!` / `eprint!`,
//! fix-r4/r5) for the branches no portable child can reach (the
//! accept-error line has no
//! deterministic fd-exhaustion trigger, and a panic inside the entry's
//! spawned connection task is contained by tokio, invisible to this
//! process).

#[allow(dead_code)]
#[path = "common/source_inputs.rs"]
mod source_inputs;

use source_inputs::SourceToken;
use std::{
    collections::{HashMap, HashSet, VecDeque},
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
/// macro scan reaches although their gating — the `host/tests.rs`
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
/// with a panicking console macro (`eprintln!`, `println!`, `print!`,
/// `eprint!`; fix-r4 widened the set from `eprintln!` alone — the entry's
/// stdout banners had stayed bare on the very file whose stderr lines
/// were already fixed — and fix-r5 added the fourth macro, `eprint!`)
/// again. This is the only killable pin for
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
/// fix-r4 feeds the three console macros it widened to, and fix-r5 the
/// fourth (`eprint`) — the widened match set is
/// load-bearing per macro, not only on the stderr spelling it started
/// from, so a detector reverted to `eprintln`-alone fails here too, and
/// so does one that drops only the fix-r5 macro.
#[test]
fn assert_no_eprintln_kills_a_real_eprintln() {
    for word in ["eprintln", "println", "print", "eprint"] {
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
        // fix-r7 P1: a gated match arm whose guard compares with `<` —
        // the shape the old angle heuristic skipped too far on — must
        // still be skipped whole: its own test-only `eprintln!` sits
        // between the phantom `<`s and the arm's `,`.
        concat!(
            "fn wrap(mode: u8, low: u8, high: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        0 if low < high && high < 100 => eprintln!(\"#2558 test-only line\"),\n",
            "        _ => production_quiet(),\n",
            "    }\n",
            "}\n",
        ),
        // fix-r7 P1: a `,` inside the gated arm's turbofish must not end
        // the skip early — the paren group behind it carries the gated
        // `eprintln!`, which a premature terminator would hand back to
        // the scan and flag.
        concat!(
            "fn wrap(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        0 => collect::<Result<u8, u16>>(eprintln!(\"#2558 test-only line\")),\n",
            "        _ => production_quiet(),\n",
            "    }\n",
            "}\n",
        ),
        // fix-r8 P1: the shapes reclassified from the keyword skip — a
        // `crate::` path pattern and a raw-identifier binding — must
        // still be skipped whole as `,`-list members: the member
        // terminator ends at the arm's own `,`, past its test-only
        // `eprintln!`.
        concat!(
            "fn wrap(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        crate::CONST => eprintln!(\"#2558 test-only line\"),\n",
            "        _ => production_quiet(),\n",
            "    }\n",
            "}\n",
        ),
        concat!(
            "fn wrap(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        r#fn => eprintln!(\"#2558 test-only line\"),\n",
            "        _ => production_quiet(),\n",
            "    }\n",
            "}\n",
        ),
        // fix-r9 P2a (DS sibling#2 + DS boundary#2): the three follower
        // shapes the fix-r8 blacklist missed — a keyword-shaped Ident
        // (the `if` of a guard, with the raw spelling stripped at the
        // lexer boundary), a group, and an `@` binding — classify as
        // members now, so the skip ends at the arm's own `,` past its
        // test-only `eprintln!`, never past the production arm.
        concat!(
            "fn wrap(mode: u8, low: u8, high: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        r#fn if low < high => eprintln!(\"#2558 test-only line\"),\n",
            "        _ => production_quiet(),\n",
            "    }\n",
            "}\n",
        ),
        concat!(
            "fn wrap(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        r#fn(1) => eprintln!(\"#2558 test-only line\"),\n",
            "        _ => production_quiet(),\n",
            "    }\n",
            "}\n",
        ),
        concat!(
            "fn wrap(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        r#fn @ 1..=9 => eprintln!(\"#2558 test-only line\"),\n",
            "        _ => production_quiet(),\n",
            "    }\n",
            "}\n",
        ),
        // The whitelist's positive faces: the three followers that keep
        // the item keyword-led — `<` (`impl<T, U>` / `fn<T, U>`), `'`
        // (`impl<'a, 'b>`), and an item name `Ident` (`fn generic<T, U>`)
        // — must still skip the whole item, commas inside the header
        // included, so the gated test-only `eprintln!` is not handed back
        // to the scan.
        concat!(
            "#[cfg(test)]\n",
            "impl<T, U> Pair<T, U> {\n",
            "    fn both() { eprintln!(\"#2558 test-only line\"); }\n",
            "}\n",
            "fn prod() { production_quiet(); }\n",
        ),
        concat!(
            "#[cfg(test)]\n",
            "impl<'a, 'b> Pair<'a, 'b> {\n",
            "    fn both() { eprintln!(\"#2558 test-only line\"); }\n",
            "}\n",
            "fn prod() { production_quiet(); }\n",
        ),
        concat!(
            "#[cfg(test)]\n",
            "fn generic<T, U>(one: T, two: U) { eprintln!(\"#2558 test-only line\"); }\n",
            "fn prod() { production_quiet(); }\n",
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
        // fix-r7 P1 (the double-lineage find): the gated arm's guard
        // compares with `<` twice, the `=>` closes only one, and the old
        // angle heuristic stayed stuck at depth 1 — the arm's own `,`
        // never terminated the skip, the production arm behind it was
        // swallowed whole, and its `eprintln!` went unscanned.
        concat!(
            "fn prod(mode: u8, low: u8, high: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        0 if low < high && high < 100 => helper(),\n",
            "        _ => eprintln!(\"#2558 production line\"),\n",
            "    }\n",
            "}\n",
        ),
        // The same hole from the unbraced body side: a comparison in the
        // gated arm's expression body locks the old counter exactly like
        // a guard's.
        concat!(
            "fn prod(mode: u8, low: u8, high: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        0 => low < high,\n",
            "        _ => eprintln!(\"#2558 production line\"),\n",
            "    }\n",
            "}\n",
        ),
        // fix-r8 P1 (MM sibling#2 + DS boundary#1, double-lineage): the
        // gated arm's pattern is a `crate::` path — the old keyword
        // table's bare `crate` made the keyword-led skip ignore the arm's
        // own `,` and swallow the production arm behind it whole, so its
        // `eprintln!` went unscanned.
        concat!(
            "fn prod(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        crate::CONST => helper(),\n",
            "        _ => eprintln!(\"#2558 production line\"),\n",
            "    }\n",
            "}\n",
        ),
        // fix-r8 P1 (the raw-identifier face of the same hole): `r#fn`
        // lexes to the plain keyword spelling (the lexer strips `r#`),
        // so a raw binding pattern read as a keyword item swallowed the
        // production arm the same way.
        concat!(
            "fn prod(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        r#fn => helper(),\n",
            "        _ => eprintln!(\"#2558 production line\"),\n",
            "    }\n",
            "}\n",
        ),
        // fix-r9 P2a (DS sibling#2 + DS boundary#2): the three follower
        // shapes the fix-r8 blacklist missed — guard `if`, a group, an
        // `@` binding — must not swallow the production arm behind
        // them: the whitelist classifies them as members, so the skip
        // ends at the arm's own `,` and the production `eprintln!`
        // fails the scan.
        concat!(
            "fn prod(mode: u8, low: u8, high: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        r#fn if low < high => helper(),\n",
            "        _ => eprintln!(\"#2558 production line\"),\n",
            "    }\n",
            "}\n",
        ),
        concat!(
            "fn prod(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        r#fn(1) => helper(),\n",
            "        _ => eprintln!(\"#2558 production line\"),\n",
            "    }\n",
            "}\n",
        ),
        concat!(
            "fn prod(mode: u8) {\n",
            "    match mode {\n",
            "        #[cfg(test)]\n",
            "        r#fn @ 1..=9 => helper(),\n",
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
/// set to the panicking console macros (fix-r5 completed the set with
/// `eprint`, the newline-less stderr twin), so the same scan now polices
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

/// DS fix-r7 boundary#2, widened to all three name families by fix-r8
/// (MM sibling#1): the name exemptions of [`is_test_support_source`]
/// (`tests.rs`, `*_tests.rs`, `test_*.rs`) are anchored to declarations,
/// not the names alone. A file exempted by spelling escapes the
/// production walk, and fix-r7's pin policed only the `test_` prefix —
/// an ungated production module named `foo_tests.rs` (or `tests.rs`)
/// slipped past both the walk and the pin, the tls.rs hole shape again.
/// This pin requires every name-exempted file to be mounted by a
/// declaration that resolves to it: a `#[cfg(test)]` (or
/// `#[cfg(all(test, …))]`) `mod <stem>;` from the directory that owns
/// the declaration, a gated `#[path = "…"]` declaration whose resolved
/// path names the file, or any declaration inside a file that is itself
/// name-exempted (the child inherits the parent's anchored exemption).
/// A family file newly mounted without any of those fails here;
/// extending the exemption then means adding the gated declaration or
/// consciously growing [`is_test_support_source`] (fail-closed).
/// fix-r9 (DS sibling#1 + DS boundary#1, the double-lineage P1): the
/// anchor is the path a declaration resolves to, never the name. A
/// gated `mod tests;` in one directory used to vouch for every
/// `tests.rs` under `src/` (the stem is the most-mounted one in the
/// tree), and a `#[path]` literal compared by file name anchored every
/// same-named `*_tests.rs`, so a rogue unmounted `tests.rs` carrying a
/// production `eprintln!` passed this pin and the tree scan alike. The
/// synthetic negative self-check below pins that direction against
/// regression, and since fix-r9 the production walk's exemption
/// ([`production_rust_sources`]) shares this anchor set, so a
/// name-exempted file with no mount is scanned, not skipped.
#[test]
fn test_family_files_stay_declaration_gated() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = src_rust_sources(manifest);
    let exempted: Vec<String> = sources
        .iter()
        .filter(|relative| {
            Path::new(relative)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_test_support_source)
        })
        .cloned()
        .collect();
    // A silently broken walk or an emptied family would pass vacuously;
    // anchor each of the three spellings on a file it has today: the
    // `test_` prefix on its `lib.rs`-declared pair, the `tests.rs`
    // spelling on the channel module's test file, and the `_tests.rs`
    // suffix on the proxy's path-mounted read-boundary file.
    for anchor in [
        "src/test_process.rs",
        "src/channel/tests.rs",
        "src/http_proxy_tests.rs",
    ] {
        assert!(
            exempted.iter().any(|relative| relative == anchor),
            "#2558 the walk found no {anchor}; the family anchor is gone \
             and this pin passes vacuously: {exempted:?}"
        );
    }
    let anchors = test_support_declaration_anchors(manifest);
    // The declaration scan must resolve mounts, not collect names: the
    // crate root's `#[cfg(test)] mod test_process;` resolves to exactly
    // this file, through the crate-root directory rule.
    assert!(
        anchors.gates(&manifest.join("src/test_process.rs")),
        "#2558 the declaration scan resolved no mount for \
         `#[cfg(test)] mod test_process;`; the derived anchor set is \
         wrong and this pin passes vacuously"
    );
    // fix-r9's negative self-check: the rogue shape this pin exists for
    // — a `tests.rs` no declaration mounts (exempted by name, anchored
    // by nothing) — must NOT be gated. Without it, an anchor set
    // degraded back to name or basename comparison passes vacuously:
    // every real exempted file is mounted, so only a never-mounted path
    // can tell the difference. (If this path ever becomes a real
    // mounted module, retire the name to another never-mounted
    // spelling.)
    let rogue = manifest.join("src/anything/tests.rs");
    assert!(
        !anchors.gates(&rogue),
        "#2558 the anchor set gates {rogue:?}, a path no declaration \
         mounts; the anchor has degraded to name comparison — the tls.rs \
         escape shape this pin exists to catch"
    );
    for relative in &exempted {
        assert!(
            anchors.gates(&manifest.join(relative)),
            "#2558 {relative} escapes the production walk by its name \
             alone; a name-exempted file must be mounted by a test-gated \
             declaration that resolves to it — a `#[cfg(test)] mod <stem>;` \
             from the directory that owns the declaration, a gated `#[path]` \
             naming it, or a declaration inside another name-exempted \
             file — mount production code under a different name, or \
             grow `is_test_support_source` consciously"
        );
    }
}

/// fix-r12 (MM concurrency#1): RAII guard for the fixture tree. The
/// hand-written `remove_dir_all` was the test body's last statement, so
/// any assertion panic skipped it and leaked the whole tree — one tree
/// per panicking mutant per negctl round. `Drop` runs on unwind too, and
/// a second removal after a partial failure is swallowed (`let _`), the
/// same shape as the `impl Drop for Gateway` guard below.
struct FixtureTree {
    root: PathBuf,
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// fix-r10 (MM boundary#1/#2/#3, sibling#1/#2, simplify#1): the anchor
/// scanner resolves `mod` declarations by rustc's own module rules,
/// pinned by the minimal-tree experiment below (the before/after
/// anchor-set evidence the fix-r10 RESULT records; the rustc side of
/// the pin is the E1–E8 `--emit=metadata` experiments the same RESULT
/// logs). Every fixture target exists on disk, so the default form's
/// `is_file` check resolves and a wrong base directory is observable
/// as the sibling file staying unanchored — not just as a code change.
#[test]
fn anchor_scanner_resolves_rustc_module_semantics() {
    let root = std::env::temp_dir().join(format!("pdrs-2558-fixr11-{}", uuid::Uuid::new_v4()));
    let _fixture_tree = FixtureTree { root: root.clone() };
    let src = root.join("src");
    for directory in [
        src.join("helper_tests"),
        src.join("outer"),
        src.join("inner"),
        src.join("foo_tests"),
        src.join("custom"),
        src.join("inlbody"),
        src.join("linked_tests"),
        src.join("side_tests"),
        src.join("orphan"),
    ] {
        fs::create_dir_all(&directory).expect("#2558 creating the fixture directory");
    }
    fs::write(
        src.join("lib.rs"),
        concat!(
            "#[cfg(test)]\n",
            "mod helper_tests;\n",
            "mod outer {\n",
            "    #[cfg(test)]\n",
            "    mod nested_tests;\n",
            "}\n",
            "mod inner {\n",
            "    #[cfg(test)]\n",
            "    #[path = \"p_tests.rs\"]\n",
            "    mod p;\n",
            "}\n",
            "fn opaque() {\n",
            "    #[cfg(test)]\n",
            "    #[path = \"decoy_tests.rs\"]\n",
            "    mod d;\n",
            "}\n",
            "#[cfg(test)]\n",
            "#[cfg_attr(f, path = \"w_tests.rs\")]\n",
            "mod w_tests;\n",
            // fix-r11 A1 (DS boundary#3): a `#[path]` on an inline `mod` —
            // the body's directory is the literal's join (rustc E3).
            "#[path = \"custom\"]\n",
            "mod inl {\n",
            "    #[cfg(test)]\n",
            "    mod child_tests;\n",
            "}\n",
            // fix-r11 B4 (DS boundary#2): a `#[path]`-mounted file's own
            // children resolve from the loaded file's directory (rustc
            // E10/E10b), not from the stem directory.
            "#[cfg(test)]\n",
            "#[path = \"linked_tests.rs\"]\n",
            "mod linked;\n",
            // fix-r12 (MM boundary#1, rustc E10b): a `#[path]` literal may
            // name its target through `..` components — rustc loads the
            // normalized file, so the mount must normalize the same way or
            // the src-walk table lookup misses it and the whole subtree
            // drops out of the harvest.
            "#[cfg(test)]\n",
            "#[path = \"outer/../dot_tests.rs\"]\n",
            "mod dot;\n",
            // fix-r13 (r12 review sibling#1 == boundary#1, one finding
            // hit by both roles): the same `..` normalization at the
            // INLINE call site — the body's directory is the normalized
            // literal join, so the child's mount matches the walk's
            // table key. A distinct target (`inlbody`), because the
            // `custom` one the boundary role suggested is already
            // mounted by `inl`'s plain literal, and a degraded inline
            // join alone must stay observable here (the file-form
            // `dot` fixture keeps covering the other call site).
            "#[path = \"outer/../inlbody\"]\n",
            "mod inl2 {\n",
            "    #[cfg(test)]\n",
            "    mod child_tests;\n",
            "}\n",
            // fix-r10 sibling#2's residual rule, pinned here for its mutant:
            // a name-exempted file that is mounted (so compiled) but not
            // anchored — the production `pub mod …_tests;` shape — seeds
            // nothing until it is itself anchored.
            "pub mod side_tests;\n",
            // fix-r11 B3 (DS boundary#1, E13): a direct `#[path]` whose
            // value is not one plain string literal is malformed — rustc
            // rejects the crate, nothing loads, nothing mounts.
            "#[cfg(test)]\n",
            "#[path = concat!(\"concat_tests\", \".rs\")]\n",
            "mod concat_tests;\n",
            // fix-r11 B3 (DS sibling#1, E12b off/on): a `cfg_attr`-
            // carried non-literal `path` — with the cfg off rustc
            // never applies the attribute and the default candidates
            // load; with it on rustc rejects the crate outright, so a
            // crate that compiles realizes the default-candidate load
            // (the pin that keeps this scanner on the rustc semantics,
            // not a blanket "unreadable mounts nothing" rule).
            "#[cfg(test)]\n",
            "#[cfg_attr(f, path = concat!(\"z_tests.rs\"))]\n",
            "mod nested_concat_tests;\n",
            // fix-r11 B3 (MM sibling#2, E11): the raw-identifier spelling
            // `r#path` is the same attribute — the shared lexer normalizes
            // it, and rustc accepts it. fix-r12 (DS boundary#1): the
            // literal no longer shares the module's default-candidate
            // name, so the mount is observable only through the `r#`
            // normalization — a lexer that kept `r#path` would leave the
            // attribute NotPath and mount nothing (the paired
            // `shared-lexer-rpath-kept` mutant pins exactly that).
            "#[cfg(test)]\n",
            "#[r#path = \"rpath_mounted_tests.rs\"]\n",
            "mod rpath;\n",
            // fix-r10 simplify#1's merge half: two readable `#[path]`
            // literals on one declaration are unreadable — rustc rejects
            // the duplicate, which literal wins is not statically provable.
            "#[cfg(test)]\n",
            "#[path = \"dupe_a_tests.rs\"]\n",
            "#[path = \"dupe_b_tests.rs\"]\n",
            "mod dupe;\n",
        ),
    )
    .expect("#2558 writing the fixture lib.rs");
    fs::write(src.join("helper_tests.rs"), "mod child_tests;\n")
        .expect("#2558 writing the fixture helper_tests.rs");
    fs::write(src.join("foo_tests.rs"), "#[cfg(test)]\nmod bar_tests;\n")
        .expect("#2558 writing the fixture foo_tests.rs");
    // fix-r11 B1 (MM boundary#1): a file no reachable declaration mounts
    // is not compiled — its gated declaration is a phantom and anchors
    // nothing.
    fs::write(src.join("orphan.rs"), "#[cfg(test)]\nmod orphan_tests;\n")
        .expect("#2558 writing the fixture orphan.rs");
    fs::write(src.join("linked_tests.rs"), "mod linked_child_tests;\n")
        .expect("#2558 writing the fixture linked_tests.rs");
    fs::write(
        src.join("side_tests.rs"),
        "#[cfg(test)]\nmod side_child_tests;\n",
    )
    .expect("#2558 writing the fixture side_tests.rs");
    for relative in [
        "helper_tests/child_tests.rs",
        "outer/nested_tests.rs",
        "nested_tests.rs",
        "inner/p_tests.rs",
        "p_tests.rs",
        "decoy_tests.rs",
        "w_tests.rs",
        "foo_tests/bar_tests.rs",
        "custom/child_tests.rs",
        "orphan/orphan_tests.rs",
        "linked_tests/linked_child_tests.rs",
        "linked_child_tests.rs",
        "dot_tests.rs",
        "inlbody/child_tests.rs",
        "side_tests/side_child_tests.rs",
        "concat_tests.rs",
        "nested_concat_tests.rs",
        "rpath_mounted_tests.rs",
        "dupe_a_tests.rs",
        "dupe_b_tests.rs",
    ] {
        fs::write(src.join(relative), "")
            .unwrap_or_else(|error| panic!("#2558 writing the fixture {relative}: {error}"));
    }
    let anchors = test_support_declaration_anchors(&root);
    let gates = |relative: &str| anchors.gates(&src.join(relative));
    // boundary#1: an un-attributed `mod x;` inside an anchored
    // name-exempted file is an anchored mount — the child inherits the
    // parent exemption's gate through the anchor fixpoint.
    assert!(
        gates("helper_tests.rs"),
        "#2558 `#[cfg(test)] mod helper_tests;` resolved no mount; the anchor set is wrong"
    );
    assert!(
        gates("helper_tests/child_tests.rs"),
        "#2558 the un-attributed `mod child_tests;` inside the anchored \
         `helper_tests.rs` mounted nothing; fix-r10 boundary#1 regressed"
    );
    // boundary#2: an inline `mod outer { … }` body is the module
    // directory `<dir>/outer`, visited exactly once — the declaration
    // inside it resolves there, and the parent directory's same-named
    // sibling (the double-visit's wrong mount) is not anchored.
    assert!(
        gates("outer/nested_tests.rs"),
        "#2558 the inline body's declaration did not resolve to \
         `<dir>/outer/nested_tests.rs`; fix-r10 boundary#2 regressed"
    );
    assert!(
        !gates("nested_tests.rs"),
        "#2558 the anchor set gates `nested_tests.rs`, a path only the \
         double-visit's parent-directory scan could mount; fix-r10 \
         boundary#2 regressed"
    );
    // boundary#3 (+ sibling#1): a `#[path]` literal inside an inline
    // `mod` body joins that inline module's directory, not the
    // declaring file's — the file rustc loads is the file mounted.
    assert!(
        gates("inner/p_tests.rs"),
        "#2558 the nested `#[path]` did not join the inline module's \
         directory; fix-r10 boundary#3 regressed"
    );
    assert!(
        !gates("p_tests.rs"),
        "#2558 the anchor set gates `p_tests.rs`, the declaring-file-dir \
         join rustc does not load; fix-r10 boundary#3 regressed"
    );
    // sibling#1: a `#[path] mod` inside a non-module context (a function
    // body) mounts nothing — the base directory there is an
    // expansion-context guess the #1180-audited shared scanner rejects,
    // and this scanner aligns with that rejection (fail-closed).
    assert!(
        !gates("decoy_tests.rs"),
        "#2558 the anchor set gates `decoy_tests.rs`, a decoy only a \
         non-module-context `#[path]` guess could mount; fix-r10 \
         sibling#1 regressed"
    );
    // simplify#1: a `path` key this scanner cannot resolve statically
    // (here `cfg_attr`-carried) mounts nothing — no fail-open collapse
    // into the default-candidate branch. fix-r12 (MM sibling#4, rustc
    // e14): for a PLAIN literal this is the deliberate exemption, pinned
    // by this assertion. The e14 experiments measured the two branches:
    // with the cfg off rustc compiles the default candidate (E0583 names
    // `src/m_tests.rs`), with it on the literal's file — which file a
    // compiling crate loads depends on cfg evaluation, so mounting
    // nothing errs strict (an unmounted name-exempted file stays in the
    // production walk — a false FAIL, never a false PASS). Mounting the
    // union instead would exempt a literal-named production file in the
    // cfg-off build that rustc never compiled — a false PASS. Logs:
    // `_tmp/20261114-2969-fixr12/rustc-sem/e14-literal-cfgattr/`.
    assert!(
        !gates("w_tests.rs"),
        "#2558 the anchor set gates `w_tests.rs` through the unreadable \
         `cfg_attr`-carried `path` key; fix-r10 simplify#1 regressed"
    );
    // sibling#2: a name-exempted file no declaration mounts (the rogue
    // shape) anchors nothing — not itself, and nothing its own
    // declarations name, gated or not.
    assert!(
        !gates("foo_tests.rs"),
        "#2558 the anchor set gates the unmounted `foo_tests.rs`; the \
         name exemption alone exempts nothing"
    );
    assert!(
        !gates("foo_tests/bar_tests.rs"),
        "#2558 the unmounted `foo_tests.rs`'s own gated declaration \
         anchored `bar_tests.rs`; fix-r10 sibling#2 regressed"
    );
    // fix-r11 A1 (DS boundary#3, a-grade): a `#[path]` on an inline `mod`
    // mounts no file itself, but the body is the directory the literal
    // names (rustc E3: `#[path="custom"] mod m { mod n; }` loads
    // `src/custom/n.rs`) — the body's gated declaration resolves there.
    assert!(
        gates("custom/child_tests.rs"),
        "#2558 the `#[path]`-mounted inline `mod inl`'s body did not \
         resolve to `custom/child_tests.rs` (rustc E3); fix-r11 A1 regressed"
    );
    // fix-r11 B1 (MM boundary#1): a file no reachable declaration mounts
    // is not compiled, so its gated declaration is a phantom — the seed
    // set and the fixpoint now share the one compilation-closure rule.
    assert!(
        !gates("orphan/orphan_tests.rs"),
        "#2558 the unreachable `orphan.rs`'s gated declaration anchored \
         `orphan/orphan_tests.rs`; fix-r11 B1 regressed"
    );
    // fix-r11 B4 (DS boundary#2, rustc E10/E10b): a `#[path]`-mounted
    // file's own children resolve from the loaded file's directory, the
    // #1180-audited rule — not from the stem directory a default-mounted
    // file would use.
    assert!(
        gates("linked_tests.rs"),
        "#2558 the gated `#[path = \"linked_tests.rs\"]` mounted nothing; \
         the anchor set is wrong"
    );
    assert!(
        gates("linked_child_tests.rs"),
        "#2558 the `#[path]`-mounted `linked_tests.rs`'s child did not \
         resolve from the loaded file's own directory (rustc E10); fix-r11 \
         B4 regressed"
    );
    assert!(
        !gates("linked_tests/linked_child_tests.rs"),
        "#2558 the anchor set gates `linked_tests/linked_child_tests.rs`, \
         the stem-directory join rustc does not load; fix-r11 B4 regressed"
    );
    // fix-r12 (MM boundary#1, rustc E10b): a `#[path]` literal with `..`
    // components names the normalized file — rustc loads it, so the mount
    // must normalize the same way or the src-walk table lookup misses it
    // and the whole subtree drops out of the harvest.
    assert!(
        gates("dot_tests.rs"),
        "#2558 the `..`-bearing `#[path]` literal's mount did not normalize \
         to the file rustc compiles (rustc E10b); fix-r12 boundary#1 regressed"
    );
    // fix-r13 (r12 review sibling#1 == boundary#1, one finding hit by
    // both roles): the same `..` normalization at the inline-body call
    // site. Reverting just that join (keeping the file-form one above)
    // left every assertion and all 28 mutants green at fix-r12 — the
    // inline `#[path]` literals were all dot-free, so the regression
    // was unobservable. The paired `inline-dot-normalize-dropped`
    // mutant pins exactly that revert; this assertion is its kill
    // point (placed after the file-form one so the global
    // `dot-dot-mount-text-joined` mutant still dies there first).
    assert!(
        gates("inlbody/child_tests.rs"),
        "#2558 the inline body's `..`-bearing `#[path]` literal did not \
         normalize its mount to the file rustc compiles (rustc E10b); \
         fix-r13 sibling#1/boundary#1 regressed"
    );
    // fix-r10 sibling#2's residual rule, pinned for its mutant: a
    // name-exempted file that is mounted (compiled) but not anchored —
    // the production `pub mod …_tests;` shape — seeds nothing.
    assert!(
        !gates("side_tests/side_child_tests.rs"),
        "#2558 the reachable-but-unanchored `side_tests.rs`'s gated \
         declaration anchored `side_child_tests.rs`; a name-exempted file \
         seeds nothing until it is itself anchored (fix-r10 sibling#2)"
    );
    // fix-r11 B3 (DS boundary#1, rustc E13): a direct `#[path]` whose
    // value is not one plain string literal mounts nothing.
    assert!(
        !gates("concat_tests.rs"),
        "#2558 the malformed `#[path = concat!(…)]` mounted \
         `concat_tests.rs`; rustc rejects the value (E13) and nothing \
         loads; fix-r11 B3 regressed"
    );
    // fix-r11 B3 (DS sibling#1, rustc E12b off/on): a `cfg_attr`-carried
    // non-literal `path` — with the cfg off rustc never applies the
    // attribute and the default candidates load, so they mount; with it
    // on rustc rejects the crate, so a crate that compiles realizes
    // the off case. The pin keeps this scanner on the rustc semantics;
    // a blanket "unparseable mounts nothing" would miss the file rustc
    // actually compiles.
    assert!(
        gates("nested_concat_tests.rs"),
        "#2558 the `cfg_attr`-carried non-literal `path` was treated as \
         unreadable and mounted nothing; with the cfg off rustc never \
         applies it and the default candidates load (E12b) — with it on, \
         the crate is rejected; fix-r11 B3 regressed"
    );
    // fix-r11 B3 (MM sibling#2, rustc E11): the raw-identifier spelling
    // `r#path` names the same attribute — observable through the shared
    // lexer's normalization because the literal is not one of the
    // module's default candidates (fix-r12, DS boundary#1).
    assert!(
        gates("rpath_mounted_tests.rs"),
        "#2558 the raw-identifier `r#path` spelling did not resolve like \
         `path` (rustc E11); fix-r11 B3 regressed"
    );
    // fix-r10 simplify#1's merge half: two readable `#[path]` literals
    // on one declaration merge to unreadable — nothing mounts.
    assert!(
        !gates("dupe_a_tests.rs"),
        "#2558 the anchor set gates `dupe_a_tests.rs` though the \
         declaration carries `#[path]` twice; fix-r10's merge regressed"
    );
    assert!(
        !gates("dupe_b_tests.rs"),
        "#2558 the anchor set gates `dupe_b_tests.rs` though the \
         declaration carries `#[path]` twice; fix-r10's merge regressed"
    );
}

/// The declaration anchors that mount a name-exempted test-support file
/// (see [`test_family_files_stay_declaration_gated`]): the set of actual
/// file paths that some `mod <name>;` declaration resolves to. fix-r9
/// (DS sibling#1 + DS boundary#1, the double-lineage P1): the anchor is
/// the resolved path, never the name — a gated `mod tests;` in one
/// directory used to vouch for every `tests.rs` under `src/`, and a
/// `#[path]` literal compared by file name anchored every same-named
/// `*_tests.rs`, so an unmounted rogue escaped both this pin and the
/// production walk. fix-r10 (MM sibling#2): a name-exempted file's own
/// declarations count only through the anchor fixpoint
/// ([`test_support_declaration_anchors`]) once the file is itself
/// anchored — the name alone exempts nothing, there too. Declarations
/// are resolved with the rustc module-resolution rules fix-r10 pinned
/// (the E1–E8 experiments in the fix-r10 RESULT): a plain `mod x;` in
/// a file names `<module dir>/x.rs` (or `<module dir>/x/mod.rs`), a
/// `#[path = "…"]` literal at the file's top level joins the declaring
/// file's own directory, and inside an inline `mod` body it joins that
/// inline module's directory (the containing module, The Rust
/// Reference "Modules / file paths").
#[derive(Default)]
struct TestSupportAnchors {
    mounted_files: HashSet<PathBuf>,
}

impl TestSupportAnchors {
    /// Mount a file path a gated (or exemption-inheriting) declaration
    /// resolves to.
    fn mount(&mut self, file: PathBuf) {
        self.mounted_files.insert(file);
    }

    /// Whether a name-exempted file is anchored by this set: the file's
    /// full path is one a declaration resolved to. Path equality, not
    /// name equality — the fix-r9 rule.
    fn gates(&self, file: &Path) -> bool {
        self.mounted_files.contains(file)
    }
}

/// The module directory that owns `file`'s child declarations: the
/// rustc rule (`source_inputs::push_target`, #1180-audited) with the
/// crate-root correction `source_module_dir` does not make on its own —
/// `lib.rs` and `main.rs` own their parent directory, while
/// `source_module_dir` keeps them in the dual seed-union context's
/// `src/lib/` shape. `mod.rs` already returns the file's own directory
/// through the shared helper.
fn declaration_module_dir(file: &Path) -> PathBuf {
    match file.file_name().and_then(|name| name.to_str()) {
        Some("lib.rs" | "main.rs") => file.parent().map(Path::to_path_buf).unwrap_or_default(),
        _ => source_inputs::source_module_dir(file),
    }
}

/// One file's `mod <name>;` harvest, split by what gated the
/// declaration (fix-r10, MM sibling#2) and by how the target was named
/// (fix-r11 B4, DS boundary#2): `gated` holds the targets of
/// declarations under a test gate — `#[cfg(test)]`,
/// `#[cfg(all(test, …))]`, or a gate inherited from a gated inline
/// `mod` body — `every` holds the targets of all declarations, gated or
/// not, and `path_mounted` holds the targets a `#[path = "…"]` literal
/// loaded: those files' own children resolve from the loaded file's own
/// directory (rustc E10/E10b, the #1180-audited rule), while the others
/// keep [`declaration_module_dir`]. A gated declaration anchors its
/// target in any file a green build compiles; an ungated one counts
/// only through the fixpoint in [`test_support_declaration_anchors`],
/// from a file that is itself anchored.
#[derive(Default)]
struct FileModMounts {
    gated: HashSet<PathBuf>,
    every: HashSet<PathBuf>,
    path_mounted: HashSet<PathBuf>,
}

/// Walk the crate sources and collect every file a `mod <name>;`
/// declaration resolves to, then close the anchor set over the name
/// exemption's inheritance (fix-r10, MM sibling#2 + boundary#1; fix-r11
/// B1/B4):
///
/// * harvest — a breadth-first walk of the mount edges out of the
///   crate roots (`src/lib.rs`, `src/main.rs`). fix-r11 B1 (MM
///   boundary#1): a file no reachable declaration mounts is never
///   harvested at all — rustc never compiles it, so its declarations,
///   gated or not, are phantom. This is the same compilation-closure
///   rule the fixpoint's "an unmounted `tests.rs` is not compiled"
///   reasoning always used; the seeds no longer get a looser one.
///   fix-r11 B4 (DS boundary#2): the per-file directory the walk
///   resolves a file's own plain `mod x;` children from is the loaded
///   file's own directory when a `#[path]` literal mounted it
///   (`path_mounted`), and `declaration_module_dir` otherwise;
/// * seeds — every test-gated declaration's target, from any harvested
///   (so compiled) file that is not itself name-exempted. A gated
///   declaration inside a name-exempted file seeds nothing until the
///   file is anchored: until then its mounts are not provably
///   test-only code (the production `pub mod …_tests;` shape);
/// * fixpoint — an anchored name-exempted file contributes every
///   declaration it holds (fix-r10 boundary#1: a plain `mod x;`
///   inside an anchored `tests.rs` is an anchored mount), and files
///   newly anchored this way contribute in turn, until the set stops
///   growing. The name exemption alone exempts nothing, here too.
fn test_support_declaration_anchors(manifest: &Path) -> TestSupportAnchors {
    let mut parsed: Vec<(PathBuf, Vec<SourceToken>)> = Vec::new();
    for relative in src_rust_sources(manifest) {
        let Ok(source) = fs::read_to_string(manifest.join(&relative)) else {
            // A non-UTF-8 payload is not Rust source (rustc cannot
            // compile it either); it cannot hold a declaration.
            continue;
        };
        let tokens = source_inputs::lex_source(&source)
            .unwrap_or_else(|error| panic!("#2558 lexing {relative}: {error}"));
        parsed.push((manifest.join(&relative), tokens));
    }
    // The crate roots — the only src files rustc compiles without a
    // `mod` edge naming them. `src/bin/*.rs` roots would belong here
    // too; this crate has none. A file mounted from outside `src/` (an
    // integration test's `#[path]`) is compiled into that other crate,
    // not this one's production walk.
    let mut queue: VecDeque<(PathBuf, PathBuf)> = parsed
        .iter()
        .filter(|(file, _)| {
            *file == manifest.join("src/lib.rs") || *file == manifest.join("src/main.rs")
        })
        .map(|(file, _)| (file.clone(), declaration_module_dir(file)))
        .collect();
    let mut harvested: HashMap<PathBuf, FileModMounts> = HashMap::new();
    while let Some((file, dir)) = queue.pop_front() {
        // A file two different edges name (rustc rejects the duplicate
        // module) keeps the first visit's directory: deterministic in
        // the harvest order, and no real tree shape reaches it.
        if harvested.contains_key(&file) {
            continue;
        }
        let Some((_, tokens)) = parsed.iter().find(|(candidate, _)| candidate == &file) else {
            // A mount target outside the src walk (a `#[path]` literal
            // naming a file elsewhere) is not this crate's source; it
            // holds no declarations for this walk.
            continue;
        };
        let file_dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut mounts = FileModMounts::default();
        collect_mod_declarations(
            tokens,
            &file,
            dir,
            &PathBase::Module(file_dir),
            false,
            &mut mounts,
        );
        let mut targets: Vec<&PathBuf> = mounts.every.iter().collect();
        targets.sort();
        for target in targets {
            let child_dir = if mounts.path_mounted.contains(target) {
                target.parent().map(Path::to_path_buf).unwrap_or_default()
            } else {
                declaration_module_dir(target)
            };
            queue.push_back((target.clone(), child_dir));
        }
        harvested.insert(file, mounts);
    }
    let mut anchored: HashSet<PathBuf> = harvested
        .iter()
        .filter(|(file, _)| {
            !file
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_test_support_source)
        })
        .flat_map(|(_, mounts)| mounts.gated.iter().cloned())
        .collect();
    loop {
        let before = anchored.len();
        for (file, mounts) in &harvested {
            let exempted = file
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_test_support_source);
            if exempted && anchored.contains(file) {
                anchored.extend(mounts.every.iter().cloned());
            }
        }
        if anchored.len() == before {
            break;
        }
    }
    let mut anchors = TestSupportAnchors::default();
    for target in anchored {
        anchors.mount(target);
    }
    anchors
}

/// Where a `#[path = "…"]` literal resolves from (fix-r10 boundary#3 +
/// sibling#1, pinned against rustc with the `--emit=metadata`
/// experiments the fix-r10 RESULT records): a module context — the
/// file's top level, where rustc joins the literal to the declaring
/// file's own directory, or an inline `mod` body, where it joins that
/// inline module's directory (the containing module, The Rust
/// Reference "Modules / file paths") — or a non-module context (a
/// function body, a macro body, a macro argument list), where rustc
/// joins the file's own directory but only from the expansion
/// context, a guess the #1180-audited shared scanner
/// (`source_inputs.rs`) rejects; this scanner aligns with that
/// rejection: no mount, fail-closed. The one divergence from the
/// shared scanner — an inline-`mod`-body `#[path]` is resolved here,
/// not rejected — is what boundary#3 asks for: the module-directory
/// base is pinned against rustc, so the mount is the file rustc
/// loads, never a decoy.
enum PathBase {
    Module(PathBuf),
    Opaque,
}

/// Collect a token slice's `mod <name>;` declarations into `mounts`.
/// fix-r10 boundary#1: declaration recognition no longer depends on a
/// `#` prefix — an un-attributed `mod x;` or `pub mod x;` is a
/// declaration too, the attributes only resolve the gate and the
/// `#[path]` literal. fix-r10 boundary#2: an inline `mod <name> {
/// … }` body is visited exactly once, through this declaration's own
/// body directory — the generic group scan never re-enters it — with
/// the chain's gate inherited into it.
fn collect_mod_declarations(
    tokens: &[SourceToken],
    file: &Path,
    dir: PathBuf,
    path_base: &PathBase,
    gated: bool,
    mounts: &mut FileModMounts,
) {
    let mut index = 0;
    while index < tokens.len() {
        let opens_declaration = match &tokens[index] {
            SourceToken::Punct('#') => matches!(
                tokens.get(index + 1),
                Some(SourceToken::Group {
                    delimiter: proc_macro2::Delimiter::Bracket,
                    ..
                })
            ),
            SourceToken::Ident(word) => word == "pub" || word == "mod",
            _ => false,
        };
        if opens_declaration {
            if let Some(next) =
                walk_mod_declaration(tokens, index, path_base, &dir, file, gated, mounts)
            {
                index = next;
            } else {
                index += 1;
            }
            continue;
        }
        // Any other group — a function or macro body, a macro argument
        // list — is a non-module context: its declarations resolve only
        // from the expansion context, so nothing inside it mounts
        // (fail-closed, aligned with the #1180 shared scanner).
        if let SourceToken::Group { tokens: inner, .. } = &tokens[index] {
            collect_mod_declarations(inner, file, dir.clone(), &PathBase::Opaque, gated, mounts);
        }
        index += 1;
    }
}

/// Walk one declaration chain from `start` — the `#` of an attribute
/// chain, or the `pub` / `mod` keyword itself — to the `mod` it names,
/// recording the resolved target (the file form) or recursing into
/// the body (the inline form). Returns the index past the
/// declaration's `;` or brace group when a `mod` was consumed — so the
/// caller steps over the inline body instead of re-scanning it
/// (fix-r10 boundary#2) — or `None` when the tokens at `start` do not
/// open a `mod` declaration.
fn walk_mod_declaration(
    tokens: &[SourceToken],
    start: usize,
    path_base: &PathBase,
    dir: &Path,
    file: &Path,
    gated: bool,
    mounts: &mut FileModMounts,
) -> Option<usize> {
    let mut scan = start;
    let mut declaration_gated = gated;
    let mut path_attr = PathAttribute::NotPath;
    loop {
        match tokens.get(scan) {
            Some(SourceToken::Punct('#')) => {
                let Some(SourceToken::Group {
                    delimiter: proc_macro2::Delimiter::Bracket,
                    tokens: attribute,
                }) = tokens.get(scan + 1)
                else {
                    return None;
                };
                if is_test_gating_predicate(attribute) {
                    declaration_gated = true;
                }
                path_attr = path_attr.merge(path_attribute_state(attribute));
                scan += 2;
            }
            Some(SourceToken::Ident(word)) if word == "pub" => {
                scan += 1;
                if matches!(
                    tokens.get(scan),
                    Some(SourceToken::Group {
                        delimiter: proc_macro2::Delimiter::Parenthesis,
                        ..
                    })
                ) {
                    scan += 1;
                }
            }
            Some(SourceToken::Ident(word)) if word == "mod" => {
                match (tokens.get(scan + 1), tokens.get(scan + 2)) {
                    // The file form — `mod <name>;` — names one file.
                    (Some(SourceToken::Ident(name)), Some(SourceToken::Punct(';'))) => {
                        if let Some(target) = mod_target(path_base, dir, name, &path_attr) {
                            mounts.every.insert(target.clone());
                            if matches!(path_attr, PathAttribute::Literal(_)) {
                                // fix-r11 B4 (DS boundary#2, rustc
                                // E10/E10b): a `#[path]`-loaded file's
                                // own children resolve from the loaded
                                // file's directory — the harvest key on
                                // this mount.
                                mounts.path_mounted.insert(target.clone());
                            }
                            if declaration_gated {
                                mounts.gated.insert(target);
                            }
                        }
                        return Some(scan + 3);
                    }
                    // The inline form — `mod <name> { … }` — mounts no
                    // file, but the body is a module whose declarations
                    // the scan must reach: its directory is
                    // `<dir>/<name>` (a `#[path]`-mounted chain's body
                    // follows the literal), and the chain's gate (or
                    // the parent's) is inherited into it.
                    (
                        Some(SourceToken::Ident(name)),
                        Some(SourceToken::Group {
                            delimiter: proc_macro2::Delimiter::Brace,
                            tokens: body,
                        }),
                    ) => {
                        let (body_dir, body_base) = match (&path_attr, path_base) {
                            (PathAttribute::Literal(literal), PathBase::Module(base)) => {
                                let mounted = normalize_mounted(&base.join(literal));
                                (mounted.clone(), PathBase::Module(mounted))
                            }
                            (_, PathBase::Module(_)) => {
                                let nested = dir.join(name);
                                (nested.clone(), PathBase::Module(nested))
                            }
                            // A non-module context stays non-module for
                            // the bodies it opens: nothing inside
                            // mounts, so the directory is inert.
                            (_, PathBase::Opaque) => (dir.join(name), PathBase::Opaque),
                        };
                        collect_mod_declarations(
                            body,
                            file,
                            body_dir,
                            &body_base,
                            declaration_gated,
                            mounts,
                        );
                        return Some(scan + 3);
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
}

/// The file a `mod <name>;` declaration names, under the rustc rules
/// fix-r10 pinned (the RESULT's E1–E8 `--emit=metadata` experiments):
/// in a module context a plain form names `<dir>/<name>.rs` or
/// `<dir>/<name>/mod.rs` (only an existing target is mounted), and a
/// readable `#[path]` literal joins the context's base — the declaring
/// file's own directory at the file top level, the inline module's
/// directory inside an inline body. A `path` key that cannot be
/// resolved statically mounts nothing (fix-r10 simplify#1: three
/// states, no fail-open collapse into the default-candidate branch),
/// and a non-module context mounts nothing at all: the plain file form
/// is rustc-invalid there, and its `#[path]` is rejected with the
/// #1180 shared scanner (the expansion-context base is not statically
/// provable).
fn mod_target(
    path_base: &PathBase,
    dir: &Path,
    name: &str,
    path_attr: &PathAttribute,
) -> Option<PathBuf> {
    match path_base {
        PathBase::Opaque => None,
        PathBase::Module(base) => match path_attr {
            PathAttribute::Literal(literal) => Some(normalize_mounted(&base.join(literal))),
            PathAttribute::Unreadable => None,
            PathAttribute::NotPath => [
                dir.join(format!("{name}.rs")),
                dir.join(name).join("mod.rs"),
            ]
            .into_iter()
            .find(|candidate| candidate.is_file()),
        },
    }
}

/// fix-r12 (MM boundary#1, rustc E10b): a `#[path]` literal may name its
/// target through `..` / `.` components (`#[path = "outer/../dot.rs"]`) —
/// rustc loads the normalized file, so the mount is normalized the same
/// way, keeping "the files rustc compiles" and "the files the src walk
/// sees" on one comparison basis (the harvest's table lookup and the
/// anchored set both match on the normalized path). Leading `..` only
/// occurs for targets outside the walk, where the lookup's `continue` is
/// the registered outside-`src/` behavior anyway.
fn normalize_mounted(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// The `#[path]` state of one attribute (fix-r10 simplify#1): three
/// states — no `path` key, a readable plain-string literal, and a key
/// this scanner cannot resolve statically. fix-r9 collapsed the last
/// into the first, so an unreadable literal fell through to the
/// default-candidate branch (fail-open, contradicting the fail-closed
/// rule the code claimed); fix-r10 splits them.
enum PathAttribute {
    NotPath,
    Literal(String),
    Unreadable,
}

impl PathAttribute {
    /// A chain's attributes merge conservatively: any unreadable
    /// `path` key stays unreadable, and two readable ones are
    /// unreadable too (rustc rejects a `mod` carrying `#[path]`
    /// twice; which literal wins is not statically provable).
    fn merge(self, other: PathAttribute) -> PathAttribute {
        match (self, other) {
            (PathAttribute::Unreadable, _) | (_, PathAttribute::Unreadable) => {
                PathAttribute::Unreadable
            }
            (PathAttribute::Literal(_), PathAttribute::Literal(_)) => PathAttribute::Unreadable,
            (PathAttribute::NotPath, other) => other,
            (current, PathAttribute::NotPath) => current,
        }
    }
}

/// Classify one attribute's `path` key (fix-r11 B3, MM sibling#2 + DS
/// boundary#1 + DS sibling#1, all three shapes pinned against rustc):
/// `#[path = "<plain string>"]` at the attribute's own top level is
/// readable; a direct `#[path = …]` whose value is anything else — a
/// multi-token expression such as `concat!(…)`, a non-plain literal —
/// is malformed input rustc rejects outright (E13), so it is
/// unreadable and mounts nothing; and a `path = "<string>"` key the
/// attribute carries conditionally — `#[cfg_attr(…, path = "…")]`,
/// nested the same way — names a file only cfg evaluation can settle
/// (the shared scanner's #1166/#1249 rule), so it is unreadable too.
/// `r#path` needs no arm of its own: the shared lexer normalizes raw
/// identifiers at the boundary (`ident_text` strips `r#`), and rustc
/// accepts the spelling as the same attribute (E11).
fn path_attribute_state(attribute: &[SourceToken]) -> PathAttribute {
    match attribute {
        [SourceToken::Ident(key), SourceToken::Punct('='), rest @ ..] if key == "path" => {
            match rest {
                [SourceToken::Literal(literal)] => {
                    match source_inputs::plain_string_value(literal) {
                        Some(value) => PathAttribute::Literal(value),
                        None => PathAttribute::Unreadable,
                    }
                }
                _ => PathAttribute::Unreadable,
            }
        }
        _ if carries_conditional_path_key(attribute) => PathAttribute::Unreadable,
        _ => PathAttribute::NotPath,
    }
}

/// Whether the tokens hold a `path = "<plain string>"` key at any
/// position other than the attribute's own top level: the
/// `cfg_attr(..., path = "…")` shape (nested the same way) and metadata
/// literals (`databake(path = "child.rs")`, #1249). None of these can
/// be resolved statically, so the caller treats them as unreadable.
/// fix-r11 B3 (DS sibling#1, rustc E12b): the value must be a plain
/// string literal for the key to count at all — a `cfg_attr`-carried
/// non-literal `path` (say `path = concat!(…)`) with the cfg off is
/// never applied by rustc and the declaration's default candidates
/// load, and with the cfg on rustc rejects the crate outright, so a
/// crate that compiles realizes the default-candidate load — it is
/// deliberately not a key here. The pinned fixture in
/// [`anchor_scanner_resolves_rustc_module_semantics`] keeps this on
/// the rustc semantics instead of a blanket "unparseable mounts
/// nothing" rule, which would miss the file rustc actually compiles.
/// fix-r12 (DS simplify#1): the walk itself is the shared scanner's
/// [`source_inputs::attribute_path_keys_in`] — one walker for every
/// consumer — with this scanner's own plainness rule re-checked in the
/// visit (the shared walker distinguishes key position, not value
/// plainness; this rule is the E12b pin's own, so it stays here where
/// its paired mutant degrades it).
fn carries_conditional_path_key(tokens: &[SourceToken]) -> bool {
    let mut found = false;
    source_inputs::attribute_path_keys_in(tokens, true, &mut |key_tokens, index, _| {
        if matches!(
            key_tokens.get(index + 2),
            Some(SourceToken::Literal(value))
                if source_inputs::plain_string_value(value).is_some()
        ) {
            found = true;
        }
    });
    found
}

/// Whether an attribute's bracket-group predicate is a test gate:
/// exactly `cfg(test)`, or `cfg(all(…))` whose list names `test`.
/// Anything else — `cfg(not(test))`, `cfg(any(test, feature))`, which
/// can hold outside a test build too — is not a gate (fail-closed).
fn is_test_gating_predicate(predicate: &[SourceToken]) -> bool {
    let [
        SourceToken::Ident(subject),
        SourceToken::Group {
            delimiter: proc_macro2::Delimiter::Parenthesis,
            tokens: arguments,
        },
    ] = predicate
    else {
        return false;
    };
    if subject != "cfg" {
        return false;
    }
    match arguments.as_slice() {
        [SourceToken::Ident(test)] => test == "test",
        [
            SourceToken::Ident(all),
            SourceToken::Group {
                delimiter: proc_macro2::Delimiter::Parenthesis,
                tokens: list,
            },
        ] => {
            all == "all"
                && list
                    .iter()
                    .any(|token| matches!(token, SourceToken::Ident(word) if word == "test"))
        }
        _ => false,
    }
}
/// `mod tests` targets (`tests.rs`, `*_tests.rs`) and the `test_*` family
/// that `lib.rs` declares under `#[cfg(test)]` (`test_process`,
/// `test_scratch`, `test_support`, ... — gated at the declaration, so the
/// files themselves carry no `#[cfg(test)]` items to skip). Every src/
/// module that is test-only by declaration today is one of these names;
/// a new spelling is not exempt, and its diagnostics fail the production
/// scan until this predicate grows (fail-closed). All three spellings
/// widen by file name alone, so they share one declaration pin —
/// [`test_family_files_stay_declaration_gated`] requires every
/// name-exempted file to be mounted by a test-gated declaration (its
/// stem, a gated `#[path]` naming it, or a declaration inside another
/// name-exempted file), so a production module that happens to carry
/// one of the three names fails the scan instead of escaping it by
/// spelling (DS fix-r7 boundary#2 pinned the `test_` prefix; fix-r8
/// completed the `tests.rs` / `*_tests.rs` families the pin had missed,
/// MM sibling#1; fix-r9, DS sibling#1 + DS boundary#1, made the mount
/// a resolved path — the name exemption alone exempts nothing).
fn is_test_support_source(name: &str) -> bool {
    name == "tests.rs" || name.ends_with("_tests.rs") || name.starts_with("test_")
}

/// Every Rust source under `src/` — including the test-support spellings
/// — as manifest-relative slash-separated paths. Deterministic order
/// (each directory's entries sorted) so a failure names the same file on
/// every run.
fn src_rust_sources(manifest: &Path) -> Vec<String> {
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

/// Every production Rust source under `src/`, as manifest-relative
/// slash-separated paths, in the lexical guards' vocabulary: all `.rs`
/// files ([`src_rust_sources`]) except the test-support spellings
/// ([`is_test_support_source`]). fix-r9: the name exemption is
/// anchor-aware — a name-exempted file is skipped only when some
/// test-gated declaration actually mounts it (the same anchor set
/// [`test_family_files_stay_declaration_gated`] pins). A name-exempted
/// file no declaration mounts (the rogue-`tests.rs` shape) stays in
/// the walk, so its `eprintln!` fails the scan instead of escaping it
/// by spelling alone.
fn production_rust_sources(manifest: &Path) -> Vec<String> {
    let anchors = test_support_declaration_anchors(manifest);
    src_rust_sources(manifest)
        .into_iter()
        .filter(|relative| {
            let file = Path::new(relative);
            let name_exempted = file
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_test_support_source);
            !(name_exempted && anchors.gates(&manifest.join(relative)))
        })
        .collect()
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
            // fix-r4: all panicking console macros, not `eprintln` alone —
            // the stdout face (a bare `println!` banner) panics on a
            // failed write exactly like the stderr spelling, so the match
            // set grew to cover it; only the match arm changed, the
            // exemption predicates and the `#[cfg(test)]` skip did not.
            // fix-r5: `eprint`, the fourth — std's console macros all
            // panic on a failed write, and the newline-less stderr twin
            // had stayed the one spelling outside the set.
            SourceToken::Ident(word)
                if word == "eprintln"
                    || word == "println"
                    || word == "print"
                    || word == "eprint" =>
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
/// so its subtree cannot hold a production error line. Which token ends
/// the item depends on the item's shape, and the two shapes need
/// different terminator rules (fix-r7, the double-lineage P1: the
/// previous single-pass angle counter treated every `<` as opening a
/// generic list, so the comparison operators of a gated match arm —
/// `#[cfg(test)] 0 if a < b && c < d => …,` — left `angle_depth` stuck at
/// 1, the arm's own `,` never terminated the skip, and the skip swallowed
/// the production arms behind it: their `eprintln!` went unscanned and
/// the guard passed on code it should have failed):
///
/// * A **keyword item** (`fn` / `mod` / `use` / `struct` / `enum` /
///   `impl` / … — see [`gated_item_is_keyword_led`]) ends at its body
///   (the first brace group) or its own `;`. Every `,` at this level sits
///   inside a generic list (`Result<TurnGateway, ProxyError>`, the
///   `assembly.rs` shape this skip exists for) or a `where` predicate,
///   so `,` is not a terminator for keyword items at all — no angle
///   counting is needed, and a where-clause comma can no longer cut the
///   skip short mid-signature either.
///
/// * A **`,`-list member** (a struct or enum-variant field, a match arm,
///   an array element) ends at its own `,`, at the first brace group, or
///   at a `;`. A `,` counts only outside generic angle lists, and in
///   member position those lists open in exactly one lexically decidable
///   way: a `<` that directly follows `::` — the turbofish, because in
///   pattern, guard, and expression position a bare `Foo<…>` is a
///   comparison to rustc too, which is why expression generics are
///   spelled `::<…>` — or a `<` nesting inside a list already open.
///   A bare `<` (`a < b` in a guard or an unbraced arm body) never locks
///   the terminator; every `>` closes one open list, saturating at zero
///   so the `>` of `->` / `=>` at depth zero stays at zero. The residual
///   imprecision is fail-closed: a `,` counted too early ends the skip
///   inside the member's own tail (say, a field type's non-turbofish
///   generic list), and the scan then reads tokens it should have
///   skipped — never fewer.
fn index_past_cfg_test_item(tokens: &[SourceToken], attribute: usize) -> usize {
    if gated_item_is_keyword_led(tokens, attribute) {
        index_past_keyword_cfg_test_item(tokens, attribute)
    } else {
        index_past_cfg_test_list_member(tokens, attribute)
    }
}

/// The gated item's keyword test: walk past the gating attribute, any
/// further attributes, and visibility / qualifier modifiers (`pub`,
/// `pub(crate)`, `async`, `unsafe`, …), then require one of the item
/// keywords followed by a token that can start the item's name or body.
/// Anything else — a field, a match arm, an array element, an
/// expression statement — classifies as a `,`-list member. Conservative
/// by design in the member direction: an unrecognized lead classifies
/// as a member, whose terminator search can only end the skip early
/// (fail-closed), never carry it past the item's end. The keyword
/// direction is not absolute: fix-r8 closed the `crate::CONST => …` and
/// `r#fn => …` shapes by blacklisting the follower puncts, and fix-r9
/// (DS sibling#2 + DS boundary#2) inverted the rule into a whitelist —
/// a real item keyword is followed only by an item name (`Ident`),
/// `extern`'s string `Literal`, or the `<` / `!` / `'` an item header
/// can open with; every other follower (a group, any other punct, a
/// keyword-shaped `Ident` such as the `if` of `r#fn if c => …`, with
/// the raw spelling normalized away at the lexer boundary) is a member
/// — the pattern binding / path head position, whose skip must end at
/// the member's own `,` instead of swallowing the production arms
/// behind it. A future keyword shape must fail closed here the same
/// way: reclassify it as a member rather than trusting the word alone.
fn gated_item_is_keyword_led(tokens: &[SourceToken], attribute: usize) -> bool {
    // The caller hands us the `#` whose bracket group is the `cfg(test)`
    // predicate, so the item starts two tokens past it.
    let mut index = attribute + 2;
    while index < tokens.len() {
        match &tokens[index] {
            // Attributes between the gate and the item (`#[cfg(test)]
            // #[inline] fn …`).
            SourceToken::Punct('#') => match tokens.get(index + 1) {
                Some(SourceToken::Group {
                    delimiter: proc_macro2::Delimiter::Bracket,
                    ..
                }) => index += 2,
                _ => return false,
            },
            SourceToken::Ident(word) => match word.as_str() {
                "pub" => {
                    index += 1;
                    if matches!(
                        tokens.get(index),
                        Some(SourceToken::Group {
                            delimiter: proc_macro2::Delimiter::Parenthesis,
                            ..
                        })
                    ) {
                        index += 1;
                    }
                }
                "async" | "unsafe" | "auto" | "default" => index += 1,
                "fn" | "mod" | "use" | "struct" | "enum" | "type" | "trait" | "impl" | "union"
                | "static" | "const" | "extern" | "macro_rules" => {
                    // fix-r9 (DS sibling#2 + DS boundary#2): a whitelist,
                    // not the fix-r8 blacklist. A real item keyword is
                    // followed only by an item name (`Ident`), a string
                    // literal (`extern "C"`), or one of the three token
                    // shapes an item header can open with: `<`
                    // (`impl<T>` / `fn<T>`), `!` (`macro_rules!`), `'`
                    // (`impl<'a>`). Everything else — a group
                    // (`r#fn(…) => …`), any other punct (`r#fn => …`,
                    // `r#fn @ … => …`, `r#fn | … => …`), or a keyword-
                    // shaped `Ident` (`r#fn if c => …` — `r#` is
                    // stripped at the lexer boundary, so the guard reads
                    // as plain `if`) — cannot start an item's name or
                    // body, so the keyword is a pattern binding or path
                    // head in member position: reclassify as a member so
                    // the skip ends at the member's own `,` instead of
                    // swallowing the production arms behind it. The
                    // misclassification cost is one-directional: a real
                    // item treated as a member only ends its skip early
                    // (more scanning, never less — fail-closed).
                    return match tokens.get(index + 1) {
                        Some(SourceToken::Literal(_)) => true,
                        Some(SourceToken::Ident(name)) => !matches!(
                            name.as_str(),
                            "if" | "else"
                                | "while"
                                | "for"
                                | "loop"
                                | "match"
                                | "break"
                                | "continue"
                                | "return"
                                | "in"
                                | "ref"
                                | "mut"
                                | "as"
                                | "where"
                                | "move"
                                | "dyn"
                                | "self"
                                | "Self"
                                | "true"
                                | "false"
                                | "await"
                        ),
                        Some(SourceToken::Punct('<' | '!' | '\'')) => true,
                        _ => false,
                    };
                }
                _ => return false,
            },
            _ => return false,
        }
    }
    false
}

/// The terminator search for a keyword-led gated item: the first brace
/// group (the item's body — parameter lists and bracketed types nest as
/// their own groups, so a brace here is the body or a `use` tree) or the
/// item's own `;`. `,` is not looked for at all; see
/// [`index_past_cfg_test_item`] for why the signature's commas are safe
/// to ignore.
fn index_past_keyword_cfg_test_item(tokens: &[SourceToken], attribute: usize) -> usize {
    let mut index = attribute;
    while index < tokens.len() {
        match &tokens[index] {
            SourceToken::Group {
                delimiter: proc_macro2::Delimiter::Brace,
                ..
            }
            | SourceToken::Punct(';') => return index + 1,
            _ => {}
        }
        index += 1;
    }
    index
}

/// The terminator search for a gated `,`-list member: its own `,` at
/// angle depth zero, the first brace group (an arm's braced body), or a
/// `;`. Only a `<` that follows `::` or nests inside an open list opens
/// one; see [`index_past_cfg_test_item`] for the full rule.
fn index_past_cfg_test_list_member(tokens: &[SourceToken], attribute: usize) -> usize {
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
            SourceToken::Punct('<') if angle_depth > 0 || follows_double_colon(tokens, index) => {
                angle_depth += 1;
            }
            SourceToken::Punct('>') => angle_depth = angle_depth.saturating_sub(1),
            _ => {}
        }
        index += 1;
    }
    index
}

/// Whether the token at `index` directly follows a `::` path separator
/// (itself two `:` puncts), the position a turbofish generic list opens
/// from in pattern / guard / expression context.
fn follows_double_colon(tokens: &[SourceToken], index: usize) -> bool {
    index >= 2
        && tokens[index - 1] == SourceToken::Punct(':')
        && tokens[index - 2] == SourceToken::Punct(':')
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
