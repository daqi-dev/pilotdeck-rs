//! Setup error adapter for the shared, metadata-preserving atomic publisher.
use super::prompt::{SetupError, SetupResult};
use crate::Failure;
use pilotdeck_cli::atomic_file;
use std::path::Path;

fn map_error(error: atomic_file::Error) -> SetupError {
    match error {
        atomic_file::Error::Refused(message) => Failure::Error(format!(
            "Cannot atomically save setup configuration: {message}."
        ))
        .into(),
        atomic_file::Error::Io {
            error,
            operation,
            path,
        } => crate::skills::fs_failure(error, &operation, &path).into(),
    }
}
pub(super) struct Target(atomic_file::Target);
impl Target {
    pub fn prepare(path: &Path) -> SetupResult<Self> {
        atomic_file::Target::prepare(path)
            .map(Self)
            .map_err(map_error)
    }
    pub fn publish(self, bytes: &[u8]) -> SetupResult<()> {
        self.0.publish(bytes).map_err(map_error)
    }
}
