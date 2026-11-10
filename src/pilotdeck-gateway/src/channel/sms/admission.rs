//! The parser-level admission call for the sms lane (#2773): extracted from
//! `route_request` into this file so the no-op negative controls own a
//! mutation target that no compiled-switch script lists as GUARDED (those
//! scripts hash `mod.rs` itself, where the call used to sit inline).

/// #2152: the TS channel is a bare `http.createServer` (SmsChannel.ts:89,
/// no options), so llhttp / `_http_server.js` reject a bad method, a
/// non-ASCII or malformed target, an over-budget header block and an
/// HTTP/1.1 request without `Host` *before* the handler runs — the same
/// gate the webhook and api-server channels already run (#1683). The
/// rejection flows through the same `finalize` as every other response.
/// The gate's `Err` is the zero-byte socket close a CONNECT to a plain
/// `http.Server` performs (measured on Node v22.22.0 against a bare
/// `createServer`: zero response bytes, then a plain close — the #2774
/// arbitration probe), the same wire outcome as the payload guard, so it
/// maps explicitly onto the connection destroy hyper already performs for
/// this error type (the serve result is discarded); the mapping stays here
/// instead of a blanket `From` so a future io error in here cannot silently
/// become a connection destroy.
pub(super) fn admit(
    request: &hyper::Request<hyper::body::Incoming>,
) -> Result<Option<hyper::Response<crate::Body>>, super::DestroyConnection> {
    crate::native_http::node_parser_admission(request).map_err(|_| super::DestroyConnection)
}
