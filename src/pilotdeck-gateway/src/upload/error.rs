//! Local counterpart of dialog/errors.ts; deliberately independent of user_dialog.
use pilotdeck_protocol::JsValue;
use std::{fmt, io, path::Path};

#[derive(Clone, Debug, PartialEq)]
pub struct UploadError {
    pub code: String,
    pub message: String,
    /// errors.ts:5 optional details; boxed to keep `Result` small.
    pub details: Option<Box<JsValue>>,
    pub(crate) dialog: bool,
}

impl UploadError {
    pub fn dialog(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            dialog: true,
        }
    }

    pub(crate) fn io(error: io::Error, operation: &str, path: Option<&Path>) -> Self {
        let path = path.map(|p| p.to_string_lossy());
        let (code, message) = crate::node_io::diagnostic(&error, operation, path.as_deref())
            .map(|(c, m)| (c.to_owned(), m))
            .unwrap_or_else(|| (String::new(), error.to_string()));
        Self {
            code,
            message,
            details: None,
            dialog: false,
        }
    }

    pub(crate) fn json(error: impl fmt::Display) -> Self {
        Self {
            code: String::new(),
            message: error.to_string(),
            details: None,
            dialog: false,
        }
    }
}
impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for UploadError {}
