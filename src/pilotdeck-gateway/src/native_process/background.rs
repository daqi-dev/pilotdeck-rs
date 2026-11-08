//! Native project-owned detached shell tasks. The execution-world family is
//! the existing rollback/disposal boundary; all sessions and SDK projections
//! clone handles to this one runtime, never create a per-turn task owner.
use crate::runtime::{
    background::BackgroundResources,
    project::{
        Families, Family, Resource, ResourceFamilySource, RuntimeResult, StageContext, StageSink,
    },
};
use std::sync::Arc;

struct NativeExecutionWorldFamily;
impl ResourceFamilySource for NativeExecutionWorldFamily {
    fn family(&self) -> Family {
        Family::ExecutionWorld
    }
    fn stage(&self, _: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        // The initial native task slice is memory-backed. Durable recovery is
        // deliberately not claimed until its disk inputs have bounded fd IO.
        let resource = Arc::new(crate::runtime::background::BackgroundExecutionWorld {
            resources: BackgroundResources::default(),
        });
        sink.register_owner(self.family(), resource.clone());
        sink.register_resource(Resource::ExecutionWorld, resource);
        Ok(())
    }
}
impl Families {
    pub(crate) fn with_native_background_tasks(mut self) -> Self {
        self.execution_world = Arc::new(NativeExecutionWorldFamily);
        self
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
