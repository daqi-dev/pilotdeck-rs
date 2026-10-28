//! SkillManager.ts:485–506 error classes plus the raw errors TS lets escape.
use crate::js_value::StackOverflow;
use pilotdeck_protocol::{JsString, JsValue};
use std::{fmt, io};

#[derive(Clone, Debug, PartialEq)]
struct ModuleFailure {
    retryability: Option<String>,
    details: Option<JsValue>,
    outcome: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillError {
    /// JS `error.name`: `SkillManagerError`, `SkillValidationError`, `Error`
    /// (a rethrown Node fs error), `SystemError` (fs.cp), `TypeError` or
    /// `RangeError` (a JS runtime fault on input).
    pub name: &'static str,
    /// SkillManagerError code, Node errno code, `ERR_*`, or empty.
    /// JS strings are UTF-16; unpaired surrogates from a module provider must
    /// survive the error projection, so both fields keep `JsString` units.
    pub code: JsString,
    pub message: JsString,
    /// SkillValidationError.validation (thrown by import).
    pub validation: Option<Box<JsValue>>,
    /// The fs errno is outside the method's measured set (node_io::lookup_for):
    /// Rust cannot vouch for Node's wording, so the host must answer.
    pub requires_host: bool,
    /// External-module failure fields, boxed so `SkillError` stays below
    /// clippy's `result_large_err` threshold for the native paths.
    failure: Option<Box<ModuleFailure>>,
    /// OS errno behind an `Error`, checked against the method's admission list.
    errno: Option<i32>,
}

impl SkillError {
    /// SkillManager.ts:489 `new SkillManagerError(code, message)`.
    pub fn manager(code: &str, message: impl Into<JsString>) -> Self {
        Self::raw("SkillManagerError", code.into(), message.into())
    }

    /// SkillManager.ts:501 `new SkillValidationError(validation)`.
    pub fn validation(validation: JsValue) -> Self {
        Self {
            validation: Some(Box::new(validation)),
            ..Self::raw(
                "SkillValidationError",
                "validation_failed".into(),
                "Validation failed.".into(),
            )
        }
    }

    /// `instanceof SkillManagerError` (SkillValidationError is a subclass).
    pub fn is_manager_error(&self) -> bool {
        matches!(self.name, "SkillManagerError" | "SkillValidationError")
    }

    /// A Node fs rejection rethrown as-is (`code: message` with syscall/path).
    pub(crate) fn io(error: &io::Error, syscall: &str, path: Option<&str>) -> Self {
        Self::errno(error, crate::node_io::diagnostic(error, syscall, path))
    }

    /// Two-path fs calls (symlink, copyfile): libuv names both, `'path' -> 'dest'`.
    pub(crate) fn io_pair(error: &io::Error, syscall: &str, path: &str, dest: &str) -> Self {
        let diagnostic = crate::node_io::diagnostic(error, syscall, Some(path))
            .map(|(code, message)| (code, format!("{message} -> '{dest}'")));
        Self::errno(error, diagnostic)
    }

    /// fs.readFile: EISDIR surfaces from `read` without a path, others from `open`.
    pub(crate) fn read_file(error: &io::Error, path: &str) -> Self {
        Self::errno(error, crate::node_io::read_file(error, path))
    }

    fn errno(error: &io::Error, diagnostic: Option<(&'static str, String)>) -> Self {
        let mut e = match diagnostic {
            Some((code, message)) => Self::raw("Error", code.into(), message.into()),
            None => Self::host(),
        };
        e.errno = error.raw_os_error();
        e
    }

    /// Node SystemError thrown by fs.cp's own checks (lib/internal/fs/cp):
    /// `${prefix}: cp returned ${errno} (${detail}) ${path}`. 待实测 against
    /// the import fixture.
    pub(crate) fn cp(code: &str, prefix: &str, errno: &str, detail: &str, path: &str) -> Self {
        Self::raw(
            "SystemError",
            code.into(),
            format!("{prefix}: cp returned {errno} ({detail}) {path}").into(),
        )
    }

    /// JS TypeError on malformed input. Wording follows V8 but is not claimed;
    /// only `name` and `code` are compared with TS.
    pub(crate) fn type_error(code: &str, message: impl Into<JsString>) -> Self {
        Self::raw("TypeError", code.into(), message.into())
    }

    /// Host-side failure with no TS counterpart (e.g. missing collation data).
    pub(crate) fn host_failure(code: &str, message: impl Into<JsString>) -> Self {
        Self::raw("Error", code.into(), message.into())
    }

    /// `protocolFailure(message)` (domainPorts.ts:193–195).
    pub(crate) fn protocol_failure(message: impl Into<JsString>) -> Self {
        Self::raw(
            "Error",
            "MODULE_PROTOCOL_INCOMPATIBLE".into(),
            message.into(),
        )
    }

    /// `moduleFailure(response, fallback)` (domainPorts.ts:184–191).
    pub(crate) fn provider_failure(
        code: impl Into<JsString>,
        message: impl Into<JsString>,
        retryability: Option<String>,
        details: Option<JsValue>,
    ) -> Self {
        Self {
            failure: Some(Box::new(ModuleFailure {
                retryability,
                details,
                outcome: None,
            })),
            ..Self::raw("Error", code.into(), message.into())
        }
    }

    /// `transportError(code, message)` (HttpModuleClient.ts:138–140).
    pub(crate) fn transport_failure(code: &str, message: impl Into<JsString>) -> Self {
        Self {
            failure: Some(Box::new(ModuleFailure {
                retryability: None,
                details: None,
                outcome: Some("result_unknown"),
            })),
            ..Self::raw("Error", code.into(), message.into())
        }
    }

    /// `moduleFailure`'s `error.retryability`.
    pub fn retryability(&self) -> Option<&str> {
        self.failure.as_ref()?.retryability.as_deref()
    }

    /// `moduleFailure`'s `error.details`.
    pub fn details(&self) -> Option<&JsValue> {
        self.failure.as_ref()?.details.as_ref()
    }

    /// `transportError`'s `outcome`.
    pub fn outcome(&self) -> Option<&'static str> {
        self.failure.as_ref()?.outcome
    }

    /// An fs outcome Rust has no measured Node wording for.
    fn host() -> Self {
        Self {
            requires_host: true,
            ..Self::raw("Error", "NODE_IO_REQUIRED".into(), JsString::default())
        }
    }

    /// A JS failure this error shape has no raw passthrough for, so the host
    /// must answer the call (`JS_HOST_REQUIRED`). Same boundary as
    /// `session_model`/`config`'s description-phase `Throws` branches.
    pub(crate) fn host_required() -> Self {
        Self {
            requires_host: true,
            ..Self::raw("Error", "JS_HOST_REQUIRED".into(), JsString::default())
        }
    }

    /// Gateway method admission (node_io::LOCAL_ERRNOS): an errno the method's
    /// oracle never observed is handed to the host instead of guessed.
    pub(crate) fn admit(self, method: &str) -> Self {
        match self.errno {
            Some(errno)
                if !self.requires_host
                    && crate::node_io::lookup_for(method, &io::Error::from_raw_os_error(errno))
                        .is_none() =>
            {
                Self::host()
            }
            _ => self,
        }
    }

    fn raw(name: &'static str, code: JsString, message: JsString) -> Self {
        Self {
            name,
            code,
            message,
            validation: None,
            requires_host: false,
            failure: None,
            errno: None,
        }
    }
}

impl From<StackOverflow> for SkillError {
    /// V8 RangeError from a too-deep array join; it has no `code`.
    fn from(_: StackOverflow) -> Self {
        Self::raw(
            "RangeError",
            JsString::default(),
            StackOverflow::MESSAGE.into(),
        )
    }
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display is diagnostic only; the lossless units live on the fields.
        f.write_str(&String::from_utf16_lossy(self.message.units()))
    }
}
impl std::error::Error for SkillError {}
