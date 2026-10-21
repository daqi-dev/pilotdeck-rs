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
