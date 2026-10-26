//! Local counterpart of dialog/errors.ts; deliberately independent of user_dialog.
use pilotdeck_protocol::{JsString, JsValue};
use std::{fmt, io, path::Path};

#[derive(Clone, Debug, PartialEq)]
pub struct UploadError {
    pub code: String,
    /// JS strings are UTF-16 code units. Messages that echo a caller string
    /// (projectKey, clientFileId) must keep lone surrogates exactly like TS;
    /// decoding them with `from_utf16_lossy` would return U+FFFD on the wire.
    /// `RequestError`/`MemoryProviderError` in this crate use the same shape.
    pub message: JsString,
    /// errors.ts:5 optional details; boxed to keep `Result` small.
    pub details: Option<Box<JsValue>>,
    pub(crate) dialog: bool,
}

impl UploadError {
    pub fn dialog(code: &str, message: impl Into<JsString>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            dialog: true,
        }
    }

    /// Human/log view only. Tests that pin the actual message compare the
    /// UTF-16 units; this helper is for `Display` and existing diagnostics.
    pub fn message_string(&self) -> String {
        String::from_utf16_lossy(self.message.units())
    }

    pub(crate) fn io(error: io::Error, operation: &str, path: Option<&Path>) -> Self {
        let path = path.map(|p| p.to_string_lossy());
        let (code, message) = crate::node_io::diagnostic(&error, operation, path.as_deref())
            .map(|(c, m)| (c.to_owned(), m))
            .unwrap_or_else(|| (String::new(), error.to_string()));
        Self {
            code,
            message: message.into(),
            details: None,
            dialog: false,
        }
    }

    pub(crate) fn json(error: impl fmt::Display) -> Self {
        Self {
            code: String::new(),
            message: error.to_string().into(),
            details: None,
            dialog: false,
        }
    }
}
impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message_string())
    }
}
impl std::error::Error for UploadError {}
