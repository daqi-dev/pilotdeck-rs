//! SkillManager.ts:485–506 error classes plus the raw errors TS lets escape.
use pilotdeck_protocol::JsValue;
use std::{fmt, io};

#[derive(Clone, Debug, PartialEq)]
pub struct SkillError {
    /// JS `error.name`: `SkillManagerError`, `SkillValidationError`, `Error`
    /// (a rethrown Node fs error) or `TypeError` (a JS runtime fault on input).
    pub name: &'static str,
    /// SkillManagerError code, Node errno code, `ERR_INVALID_ARG_TYPE`, or empty.
    pub code: String,
    pub message: String,
    /// SkillValidationError.validation (thrown by import, A6.8-pr4).
    pub validation: Option<Box<JsValue>>,
}

impl SkillError {
    /// SkillManager.ts:489 `new SkillManagerError(code, message)`.
    pub fn manager(code: &str, message: impl Into<String>) -> Self {
        Self {
            name: "SkillManagerError",
            code: code.into(),
            message: message.into(),
            validation: None,
        }
    }

    /// SkillManager.ts:501 `new SkillValidationError(validation)`.
    pub fn validation(validation: JsValue) -> Self {
        Self {
            name: "SkillValidationError",
            code: "validation_failed".into(),
            message: "Validation failed.".into(),
            validation: Some(Box::new(validation)),
        }
    }

    /// `instanceof SkillManagerError` (SkillValidationError is a subclass).
    pub fn is_manager_error(&self) -> bool {
        matches!(self.name, "SkillManagerError" | "SkillValidationError")
    }

    /// A Node fs rejection rethrown as-is (`code: message` with syscall/path).
    pub(crate) fn io(error: &io::Error, syscall: &str, path: Option<&str>) -> Self {
        let (code, message) = crate::node_io::diagnostic(error, syscall, path)
            .map(|(c, m)| (c.to_owned(), m))
            .unwrap_or_else(|| (String::new(), error.to_string()));
        Self::raw("Error", code, message)
    }

    /// fs.readFile: EISDIR surfaces from `read` without a path, others from `open`.
    pub(crate) fn read_file(error: &io::Error, path: &str) -> Self {
        match crate::node_io::read_file(error, path) {
            Some((code, message)) => Self::raw("Error", code.into(), message),
            None => Self::raw("Error", String::new(), error.to_string()),
        }
    }

    /// JS TypeError on malformed input. Wording follows V8 but is not claimed;
    /// only `name` and `code` are compared with TS.
    pub(crate) fn type_error(code: &str, message: impl Into<String>) -> Self {
        Self::raw("TypeError", code.into(), message.into())
    }

    /// Host-side failure with no TS counterpart (e.g. missing collation data).
    pub(crate) fn host(code: &str, message: String) -> Self {
        Self::raw("Error", code.into(), message)
    }

    fn raw(name: &'static str, code: String, message: String) -> Self {
        Self {
            name,
            code,
            message,
            validation: None,
        }
    }
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SkillError {}
