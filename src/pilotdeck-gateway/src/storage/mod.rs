//! Session-owned operation persistence. No gateway route or host is wired here.
//! The wire format remains the TS transcript JSONL format (no extra revision field).
//! 事件日志及完整 SessionDomainValidation 状态机由 adapter 与 recorder 共用。
//! `run_registry` is the TS RunRegistry snapshot store; it is not wired into
//! the host or gateway methods either.
pub mod file_history;
mod ledger;
mod projection;
pub mod recorder;
mod run_registry;
mod session_domain;
pub(crate) mod stimulus;
mod transcript;
mod transcript_adapter;
mod workspace_snapshot;

pub use run_registry::{
    FileRunRegistry, RunAcceptInput, RunAccepted, RunRecord, RunRef, RunRegistryError, RunState,
    unrekeyable_skips,
};
// #637 fix-r4: shared "wait for a concurrent file writer" bound; the spill
// wait derives from it rather than duplicating the 30 s.
pub(crate) use run_registry::FILE_LOCK_TIMEOUT;
pub use transcript_adapter::{
    AdapterError, AdapterResult, AsyncTranscriptAdapter, Clock, JsonlTranscriptStore, ProjectKey,
    SessionStorage, SessionStorageInput, StoreResult, StoredSession, SubagentHandle,
    SubagentReferences, TranscriptKey, TranscriptStore, TranscriptWriter,
    project_subagent_references, sanitize_session_id,
};

pub(crate) use transcript::{
    LedgerLock, MAX_BYTES as TRANSCRIPT_MAX_BYTES, canonical_transcript_path,
};
pub(crate) use transcript_adapter::slice_index;

pub use ledger::{
    KnownTerminal, OperationIdentity, OperationRecovery, OperationResolution,
    SessionOperationLedger, TranscriptSink, UnknownTerminal,
};
pub use transcript::{
    Diagnostic, JsonlLedgerStore, TranscriptRead, WriterState, read_transcript,
    read_transcript_limited,
};
pub use workspace_snapshot::{
    ContentAddressedWorkspaceSnapshotRecorder, RestoredSnapshot, SnapshotPhase, SnapshotRoundHook,
    SnapshotRoundStage, SnapshotState, WorkspaceSnapshotConfig, WorkspaceSnapshotDescriptor,
    WorkspaceSnapshotError, WorkspaceSnapshotInput, WorkspaceSnapshotProvider,
    WorkspaceSnapshotResult, create_workspace_snapshot_provider, resolve_workspace_snapshot_config,
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
            // #1554: the fs call site labels its syscall and path with
            // `node_io::path_error` / `descriptor_error`, and this renders
            // Node's `code: description, syscall 'path'` shape from the measured
            // libuv table. An error nobody labelled keeps Rust's own text: the
            // syscall cannot be recovered from a bare `io::Error`, and Display
            // must not guess one. The OS errno behind it is positive here;
            // Node reports the same number negated (`EEXIST` is -17), so
            // `node_io::os_errno` hands back the positive form.
            Self::Io(e) => match crate::node_io::staged_diagnostic(e) {
                Some((_, message)) => f.write_str(&message),
                None => e.fmt(f),
            },
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
mod stimulus_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transcript_adapter_tests;

mod task_budget;
pub use task_budget::{TaskBudgetError, TaskBudgetScope, TaskBudgetState, TaskBudgetStore};

// Fault injection shared by publication tests; production storage stays unchanged.
#[cfg(test)]
pub(crate) fn fail_next_registry_persist(registry: &FileRunRegistry) {
    registry
        .fail_persist
        .store(true, std::sync::atomic::Ordering::SeqCst);
}
