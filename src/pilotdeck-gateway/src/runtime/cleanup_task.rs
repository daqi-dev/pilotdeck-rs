//! Completion must survive a task that is never polled, aborted, or panics.
use crate::host::{HostError, ModuleError};
use std::{future::Future, panic::AssertUnwindSafe};
use tokio::{runtime::Handle, sync::watch};

struct Completion<T>(Option<watch::Sender<Option<Result<T, HostError>>>>);
impl<T> Completion<T> {
    fn finish(mut self, result: Result<T, HostError>) {
        self.0.take().unwrap().send_replace(Some(result));
    }
}
impl<T> Drop for Completion<T> {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            // No owner locks: spawn may drop the future synchronously while the
            // caller holds its state lock (for example on a stopped runtime).
            done.send_replace(Some(Err(ModuleError::new(
                "Interaction cleanup task was dropped or panicked before completion.",
            )
            .into())));
        }
    }
}

pub(crate) fn spawn<T: Send + Sync + 'static>(
    executor: &Handle,
    done: watch::Sender<Option<Result<T, HostError>>>,
    cleanup: impl Future<Output = Result<T, HostError>> + Send + 'static,
) {
    spawn_after(executor, done, || {}, cleanup);
}

pub(crate) fn spawn_after<T: Send + Sync + 'static>(
    executor: &Handle,
    done: watch::Sender<Option<Result<T, HostError>>>,
    prepare: impl FnOnce(),
    cleanup: impl Future<Output = Result<T, HostError>> + Send + 'static,
) {
    // Construct before spawning: an unpolled task must own the guard too.
    let completion = Completion(Some(done));
    // Tokio can reject a spawn during shutdown. Never unwind through the
    // synchronous resource shutdown path; dropping completion reports failure.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        prepare();
        executor.spawn(async move { completion.finish(cleanup.await) })
    }));
}
