//! Session-owned operation persistence. No gateway route or host is wired here.
//! The wire format remains the TS transcript JSONL format (no extra revision field).
//! 恢复校验仅覆盖事件日志层（validateSessionEventLog）；SessionRuntime.ts:109
//! 的 validateSessionDomainLog 领域校验留给接入 host/runtime 的后续 PR。
//! `run_registry` is the TS RunRegistry snapshot store; it is not wired into
//! the host or gateway methods either.
mod ledger;
mod projection;
mod run_registry;
mod transcript;
mod transcript_adapter;

pub use run_registry::{
    FileRunRegistry, RunAcceptInput, RunAccepted, RunRecord, RunRef, RunRegistryError, RunState,
};
pub use transcript_adapter::{
    AdapterError, AdapterResult, AsyncTranscriptAdapter, Clock, JsonlTranscriptStore, ProjectKey,
    SessionStorage, SessionStorageInput, StoreResult, StoredSession, SubagentHandle, TranscriptKey,
    TranscriptStore, TranscriptWriter, sanitize_session_id,
};

pub use ledger::{
    KnownTerminal, OperationIdentity, OperationRecovery, OperationResolution,
    SessionOperationLedger, TranscriptSink, UnknownTerminal,
};
pub use transcript::{
    Diagnostic, JsonlLedgerStore, TranscriptRead, WriterState, read_transcript,
    read_transcript_limited,
};

use std::{fmt, io};

#[derive(Debug)]
pub enum LedgerError {
    Io(io::Error),
    Invalid(String),
    /// The store can no longer certify its in-memory view. Drop the ledger and
    /// reopen the transcript before retrying, including after an uncertain commit.
    ReopenRequired(String),
}
impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Invalid(s) => f.write_str(s),
            Self::ReopenRequired(reason) => {
                write!(
                    f,
                    "Session operation ledger must be reopened before use: {reason}"
                )
            }
        }
    }
}
impl std::error::Error for LedgerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Invalid(_) | Self::ReopenRequired(_) => None,
        }
    }
}
impl From<io::Error> for LedgerError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<pilotdeck_protocol::CodecError> for LedgerError {
    fn from(e: pilotdeck_protocol::CodecError) -> Self {
        Self::Invalid(e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, LedgerError>;
fn invalid(message: impl Into<String>) -> LedgerError {
    LedgerError::Invalid(message.into())
}

#[cfg(test)]
mod run_registry_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transcript_adapter_tests;
