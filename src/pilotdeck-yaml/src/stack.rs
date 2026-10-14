//! Runtime adapter: keep the upstream recursive composition/conversion functions,
//! but do not depend on a caller's (often 2 MiB) native thread stack.
//! This does not try to emulate V8's JIT-dependent stack exhaustion threshold.
pub(crate) fn with_large_stack<T: Send>(needed: bool, f: impl FnOnce() -> T + Send) -> T {
    if !needed {
        return f();
    }
    std::thread::scope(|scope| {
        match std::thread::Builder::new()
            .name("pilotdeck-yaml".into())
            .stack_size(32 * 1024 * 1024)
            .spawn_scoped(scope, f)
            .expect("Unable to allocate YAML parsing stack")
            .join()
        {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}
