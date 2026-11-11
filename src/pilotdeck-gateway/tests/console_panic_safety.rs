//! #2558: the A4 gateway's error console writes never panic when stderr
//! fails. The #2558 sites — the A4 entry (`main.rs` startup/accept/
//! connection), `http_proxy.rs`'s four error lines, `upstream.rs`'s
//! connection task, the memory maintenance default diagnostic, the native
//! entry's listening/stop/halt lines (`src/native_process.rs`, fix-r1),
//! and the default warn sinks (`refresh.rs`, `replacement.rs`,
//! `plugins.rs`, fix-r1) — go through the shared `console_log_line`, which
//! drops the write error like TS's `console.error`, while `println!`/
//! `eprintln!` panic on a failed write (std behavior). `native_ws.rs`
//! joined the same shape in #2434. The lib-side probes (`http_proxy::tests`,
//! `upstream::tests`, `runtime::memory::maintenance::tests`, plus
//! `native_process::channels_tests`' failing-sink unit case) drive the
//! reachable sites in re-exec'd children with EPIPE stderr; this file owns
//! what only the real binary can pin — the entry's own three sites — plus
//! the lexical guard that keeps every #2558 file `eprintln!`-free for the
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
/// through the writer in #2434). Kept complete against new callers by
/// `shared_writer_call_sites_stay_listed`. Scanned through the shared
/// proc-macro2 lexer (`common/source_inputs`), never a text search:
/// comments and string literals — including the doc comments that quote
/// the macro — are token kinds there and cannot false-positive.
const EPRINTLN_FREE_SOURCES: [&str; 10] = [
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
];

/// Lexical guard: none of the #2558 files may write an error console line
/// with the panicking `eprintln!` again. This is the only killable pin for
/// the two entry lines no child can observe (the accept-error branch has no
/// portable trigger, and the spawned connection task's panic is contained
/// by tokio), and it kills every site mutation even where the behavioral
/// probes already do.
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
#[test]
fn assert_no_eprintln_kills_a_real_eprintln() {
    let fixture = "fn f() { eprintln!(\"#2558 fixture line\"); }";
    let tokens = source_inputs::lex_source(fixture)
        .unwrap_or_else(|error| panic!("#2558 lexing the self-test fixture: {error}"));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_no_eprintln(&tokens, "fixture.rs")
    }))
    .expect_err("#2558 the detector must panic on a real eprintln! site");
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

/// Every production source file that lexically names the shared writer must
/// be listed in `EPRINTLN_FREE_SOURCES` (fix-r1 review MM sibling#4's
/// derived rule): the guard is the only killable pin for sites without a
/// portable EPIPE trigger, so an unlisted caller would silently escape it
/// — exactly how `native_ws.rs` (a #2434 caller) sat outside the table
/// until fix-r1. Naming the writer counts a call, the definition, or a
/// re-export; comments and doc text quote it only as non-`Ident` tokens.
/// Test modules (`tests.rs` / `*_tests.rs`, the crate's two test-file
/// spellings) are excluded: their diagnostics are libtest-support code,
/// not product console lines.
#[test]
fn shared_writer_call_sites_stay_listed() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut derived = Vec::new();
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
            let is_test_module = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"));
            if is_test_module {
                continue;
            }
            let bytes = fs::read(&path)
                .unwrap_or_else(|error| panic!("#2558 reading {}: {error}", path.display()));
            let Ok(source) = String::from_utf8(bytes) else {
                // A non-UTF-8 payload is not Rust source (rustc cannot
                // compile it either); it cannot name the writer.
                continue;
            };
            let tokens = source_inputs::lex_source(&source).unwrap_or_else(|error| {
                panic!(
                    "#2558 {relative} is not lexable Rust; fail closed: {error}",
                    relative = path.display()
                )
            });
            if tokens_contain_ident(&tokens, "console_log_line") {
                derived.push(
                    path.strip_prefix(manifest)
                        .expect("#2558 source sits under the manifest")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
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

fn tokens_contain_ident(tokens: &[SourceToken], word: &str) -> bool {
    tokens.iter().any(|token| match token {
        SourceToken::Ident(name) => name == word,
        SourceToken::Group { tokens, .. } => tokens_contain_ident(tokens, word),
        _ => false,
    })
}

fn assert_no_eprintln(tokens: &[SourceToken], file: &str) {
    for (index, token) in tokens.iter().enumerate() {
        match token {
            SourceToken::Ident(word) if word == "eprintln" => {
                assert!(
                    tokens.get(index + 1) != Some(&SourceToken::Punct('!')),
                    "#2558 {file} still writes an error console line with the \
                     panicking `eprintln!` (std panics when the write fails); \
                     route it through `console_log_line` instead"
                );
            }
            SourceToken::Group { tokens, .. } => assert_no_eprintln(tokens, file),
            _ => {}
        }
    }
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
