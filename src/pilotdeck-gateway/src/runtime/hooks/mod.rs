//! Session-scoped hook execution (TS `extension/hooks/execution`).
//!
//! `exec` owns the single-hook executors and `parseHookOutput`; `runtime` owns
//! match/execute/aggregate and the lifecycle dispatch adapter; `pending` owns
//! the session's pending async hooks. Persistent configuration and matching
//! live next door in `crate::hooks` (A6.7-pr3).
pub mod exec;
pub mod pending;
pub mod runtime;

pub use exec::{Cancellation, ExecutionResult, HookOutcome, parse_hook_output};
pub use pending::{
    AsyncHookCompletion, AsyncHookResponse, PendingAsyncHookDescriptor, PendingAsyncHooks,
};
pub use runtime::{
    HookEffect, HookRuntime, HookRuntimeOptions, HookRuntimeRunInput, HookRuntimeRunResult,
    LifecycleDispatchInput, LifecycleDispatchResult, LifecycleError, LifecycleRuntime,
    serialize_lifecycle_dispatch_result,
};
