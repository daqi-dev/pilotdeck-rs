//! Parsed WebSocket proxy. TS owns authentication and methods outside the verified local whitelist.
mod acceptor;
pub mod config;
mod dispatch;
mod fetch_redirect;
mod frame;
pub mod hooks;
pub mod host;
pub mod http_proxy;
mod js_value;
mod metadata;
mod model_catalog;
pub mod model_cost;
mod native_archive;
mod node_io;
mod pairing;
mod process_group;
#[cfg(all(test, unix))]
mod process_spawn;
mod project_files;
pub mod runtime;
mod session_model;
pub mod skills;
pub mod storage;
#[cfg(test)]
#[path = "../tests/common/baseline.rs"]
mod test_baseline;
#[cfg(test)]
mod test_process;
#[cfg(test)]
#[path = "../tests/common/scratch.rs"]
mod test_scratch;
pub mod upload;
mod upstream;
pub mod user_dialog;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::body::Incoming;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use std::error::Error;

pub type ProxyError = Box<dyn Error + Send + Sync>;
pub type Body = BoxBody<Bytes, ProxyError>;
pub type HttpClient = Client<HttpConnector, Incoming>;

/// The Node/libuv code and description for an OS error, when the generated
/// table knows the errno. The CLI shares this table instead of maintaining a
/// second copy of the libuv message strings.
pub fn node_fs_errno(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    node_io::lookup(error)
}

/// Read the original session bytes without transcript validation or sorting.
/// Callers retain physical line numbers and apply their own projection.
pub fn read_session_bytes(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use metadata::Files;
    metadata::NativeFiles.read(path)
}

/// The native Node-compatible directory listing used by session storage.
pub fn list_session_entries(path: &std::path::Path) -> std::io::Result<Vec<(String, bool)>> {
    use metadata::Files;
    metadata::NativeFiles.entries(path)
}

/// Resolve a chat directory through the existing project marker lookup.
pub fn session_chat_dir(
    home: &std::path::Path,
    cwd: &std::path::Path,
    project: &str,
) -> std::path::PathBuf {
    metadata::Metadata {
        home: home.into(),
        cwd: cwd.into(),
        files: metadata::NativeFiles,
    }
    .chat_dir(project)
}

/// Register this process's SIGXFSZ handler, aligning it with Node (#621).
/// Node's platform init ignores SIGXFSZ process-wide, so a write past
/// RLIMIT_FSIZE surfaces as an `EFBIG` error from the syscall; Rust std
/// ignores only SIGPIPE, so the same write instead kills the process
/// (coordinator probe, node v22.22.0 vs rustc 1.99.0 + tokio 1.48.0,
/// `ulimit -f 1` writing 4 KiB: unregistered the writing process dies with
/// exit 153; registered through tokio's safe signal API it observes
/// os error 27 and continues — evidence
/// `_tmp/20261025-issue-621-takeover/xfsz-tokio-probe/`). The gateway
/// binary calls this at startup, and the debug request dump registers
/// before its write (`runtime/model/http.rs`), so every host process of the
/// model runtime is covered — that runtime has no process entry of its own.
/// Exactly once per process (`OnceLock`); the returned `Signal` must stay
/// held so the disposition stays installed, so it lives in the lock.
/// Registration requires a tokio runtime with the signal driver enabled —
/// every runtime this crate builds in production is `enable_all` (the
/// gateway binary's `#[tokio::main]` included). Without a runtime on the
/// calling thread it degrades to a stderr note and the default disposition,
/// no worse than without this guard; inside a runtime that lacks the signal
/// driver, tokio's `signal()` panics instead of returning `Err`, so the
/// `Handle::try_current` guard in `register_sigxfsz` only catches the
/// no-runtime case.
#[cfg(unix)]
pub fn ensure_sigxfsz_ignored() {
    static SIGNAL: std::sync::OnceLock<Option<tokio::signal::unix::Signal>> =
        std::sync::OnceLock::new();
    SIGNAL.get_or_init(|| match register_sigxfsz() {
        Ok(signal) => Some(signal),
        // A failed attempt is recorded as `None`, so the registration
        // and this note both happen exactly once per process.
        Err(reason) => {
            eprintln!(
                "[model-debug] SIGXFSZ handler not registered ({reason}); \
                 writes past RLIMIT_FSIZE will terminate the process"
            );
            None
        }
    });
}

/// The registration attempt itself: safe tokio signal API, no unsafe. The
/// runtime check first keeps the library callable from plain (non-async)
/// threads, where tokio's `signal()` would panic instead of answering.
#[cfg(unix)]
fn register_sigxfsz() -> Result<tokio::signal::unix::Signal, String> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err("no tokio runtime on this thread".into());
    }
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(libc::SIGXFSZ))
        .map_err(|error| format!("tokio signal registration failed: {error}"))
}
