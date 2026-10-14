//! Downstream TS-compatible frame actions and upgrade acceptance.
use crate::{
    Body,
    frame::Frame,
    http_proxy::{empty, strip_hop_headers},
};
use hyper::{Response, header};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

pub(crate) enum Action {
    Text,
    Ping,
    Close,
    Ignore,
}

pub(crate) fn action(frame: &Frame) -> Action {
    match frame.opcode {
        1 => Action::Text,
        // TS owns control replies and their order relative to application output.
        9 => Action::Ping,
        8 => Action::Close,
        // Includes continuation, binary, pong and reserved opcodes. Neither FIN
        // nor RSV affects TS's opcode-only handling; no fragment reassembly.
        _ => Action::Ignore,
    }
}

pub(crate) fn websocket_accept(headers: &hyper::HeaderMap) -> Option<String> {
    let key = headers.get(header::SEC_WEBSOCKET_KEY)?;
    // GatewayServer.ts checks only that the key is a string, not its base64
    // length, method or protocol version. Hyper handles HTTP upgrade routing.
    Some(derive_accept_key(key.as_bytes()))
}

pub(crate) fn websocket_response(mut headers: hyper::HeaderMap, accept: &str) -> Response<Body> {
    strip_hop_headers(&mut headers);
    headers.insert(
        header::UPGRADE,
        hyper::header::HeaderValue::from_static("websocket"),
    );
    headers.insert(
        header::CONNECTION,
        hyper::header::HeaderValue::from_static("Upgrade"),
    );
    headers.insert(
        header::SEC_WEBSOCKET_ACCEPT,
        accept.parse().expect("derived accept key"),
    );
    let mut response = Response::new(empty());
    *response.status_mut() = hyper::StatusCode::SWITCHING_PROTOCOLS;
    *response.headers_mut() = headers;
    response
}
