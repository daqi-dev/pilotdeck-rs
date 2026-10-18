//! Bound non-interruptible tokenizer/JSON batches without blocking the reactor.
use crate::host::{HostError, ModuleError};
use std::sync::{Arc, OnceLock};
use tokio::{sync::Semaphore, task::JoinHandle};

struct Task<T>(JoinHandle<T>);
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
/// Dropping a call cancels queued batches. A batch already executing finishes
/// under its permit, so cancelled requests cannot create unbounded CPU work.
pub(crate) async fn run<T, F>(work: F) -> Result<T, HostError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, HostError> + Send + 'static,
{
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ModuleError::new(e.to_string()))?;
    let mut task = Task(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    }));
    (&mut task.0)
        .await
        .map_err(|e| ModuleError::new(e.to_string()))?
}
