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
    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        // The initial native task slice is memory-backed. Durable recovery is
        // deliberately not claimed until its disk inputs have bounded fd IO.
        let env = context.env();
        let temporary_root = env
            .get("TMPDIR")
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let sandbox = Arc::new(crate::runtime::tools::sandbox::NativeSandbox::new(
            context.profile().sandbox_mode,
            temporary_root,
        ));
        let resource = Arc::new(crate::runtime::background::BackgroundExecutionWorld {
            resources: BackgroundResources {
                tasks: crate::runtime::background::BackgroundTaskRuntime::default()
                    .with_sandbox(sandbox.clone()),
                subagents: Default::default(),
            },
            sandbox,
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
