//! Pure Source compact boundary and replacement projection. Host owns durability.
use crate::ProjectedMessage;
use crate::ports::{CompactBoundary, PortResult};
use crate::tool_context::ContextMessageCollection;
use pilotdeck_protocol::model::BoundarySnapshot;

pub struct CompactionPersistenceInput<'a> {
    /// The actual acknowledged Source value, including own Undefined/extensions.
    pub compact: &'a BoundarySnapshot,
    pub persistence_advertised: bool,
}

#[derive(Debug, Clone)]
pub struct CompactionPersistenceCommit {
    pub boundary: CompactBoundary,
    /// Source adds compactReplacement markers only to the callback snapshot.
    pub messages: Vec<ProjectedMessage>,
}

/// Source constructs boundary metadata before the persistence try, then marks
/// callback messages inside it. Their failure phases have different recovery.
#[derive(Debug, Clone)]
pub enum CompactionSnapshotError {
    BoundaryConstruction(crate::ports::PortError),
    PersistencePayload(crate::ports::PortError),
}

#[derive(Debug, Clone)]
pub struct CompactionPersistencePlan {
    /// Actual compact.messages, without persistence-only markers.
    pub replacement: ContextMessageCollection,
    /// Source skips persistence if its callback or compact.result is absent.
    pub commit: Result<Option<CompactionPersistenceCommit>, CompactionSnapshotError>,
}

pub struct OriginalCompactionPersistencePolicy;
impl OriginalCompactionPersistencePolicy {
    pub fn evaluate(
        &self,
        input: CompactionPersistenceInput<'_>,
    ) -> PortResult<CompactionPersistencePlan> {
        crate::compaction_persistence_policy::evaluate(input)
    }
}
