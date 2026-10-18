//! Parsed WebSocket proxy. TS owns authentication and methods outside the verified local whitelist.
mod acceptor;
mod config;
mod dispatch;
mod frame;
pub mod host;
pub mod http_proxy;
mod metadata;
mod model_catalog;
mod native_archive;
mod node_io;
mod pairing;
mod project_files;
mod session_model;
pub mod skills;
pub mod storage;
#[cfg(test)]
mod test_process;
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
