//! Admission captures are distinct from ordinary typed content clones. The
//! normalized domain is owned; immutable provider echoes are detached here too.
use crate::{
    JsValue,
    model::{BoundarySnapshot, CanonicalModelRequest, ContentBody, PreparedRequest, ProviderEcho},
};
use std::sync::Arc;

pub(crate) fn capture_request(request: &CanonicalModelRequest) -> PreparedRequest {
    let mut detached = request.clone();
    for message in &mut detached.messages {
        for block in &mut message.content {
            let raw = match &mut block.body {
                ContentBody::ToolCall(call) => &mut call.raw,
                ContentBody::ToolResult(result) => &mut result.raw,
                _ => continue,
            };
            if let Some(echo) = raw {
                *echo = ProviderEcho(Arc::new(echo.0.as_ref().clone()));
            }
        }
    }
    PreparedRequest::from_detached(detached)
}

pub(crate) fn capture_boundary(value: &JsValue) -> BoundarySnapshot {
    // JsValue's iterative clone preserves own Undefined and WTF-16, without
    // stringify's omission, null normalization or string replacement decoding.
    BoundarySnapshot::from_detached(value.clone())
}
