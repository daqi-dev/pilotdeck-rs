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
