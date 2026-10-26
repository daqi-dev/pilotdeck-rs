//! Completion must survive a task that is never polled, aborted, or panics.
use crate::host::{HostError, ModuleError};
use std::{
    any::Any,
    future::Future,
    panic::AssertUnwindSafe,
    sync::{Mutex, MutexGuard},
};
use tokio::{runtime::Handle, sync::watch};

pub(crate) struct Completion<T, E> {
    done: Option<watch::Sender<Option<Result<T, E>>>>,
    dropped_error: Option<E>,
}
impl<T, E> Completion<T, E> {
    /// Construct before spawning: an unpolled task must own the guard too.
    pub(crate) fn new(done: watch::Sender<Option<Result<T, E>>>, dropped_error: E) -> Self {
        Self {
            done: Some(done),
            dropped_error: Some(dropped_error),
        }
    }
    pub(crate) fn finish(mut self, result: Result<T, E>) {
        self.done.take().unwrap().send_replace(Some(result));
    }
}
impl<T, E> Drop for Completion<T, E> {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            // No owner locks: spawn may drop the future synchronously while the
            // caller holds its state lock (for example on a stopped runtime).
            done.send_replace(Some(Err(self.dropped_error.take().unwrap())));
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
    spawn_after_with_error(
        executor,
        done,
        ModuleError::new("Interaction cleanup task was dropped or panicked before completion.")
            .into(),
        prepare,
        cleanup,
    );
}

pub(crate) fn spawn_with_error<T: Send + Sync + 'static, E: Send + Sync + 'static>(
    executor: &Handle,
    done: watch::Sender<Option<Result<T, E>>>,
    dropped_error: E,
    cleanup: impl Future<Output = Result<T, E>> + Send + 'static,
) {
    spawn_after_with_error(executor, done, dropped_error, || {}, cleanup);
}

fn spawn_after_with_error<T: Send + Sync + 'static, E: Send + Sync + 'static>(
    executor: &Handle,
    done: watch::Sender<Option<Result<T, E>>>,
    dropped_error: E,
    prepare: impl FnOnce(),
    cleanup: impl Future<Output = Result<T, E>> + Send + 'static,
) {
    let completion = Completion::new(done, dropped_error);
    // Tokio can reject a spawn during shutdown. Never unwind through the
    // synchronous resource shutdown path; dropping completion reports failure.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        prepare();
        executor.spawn(async move { completion.finish(cleanup.await) })
    }));
}

/// Spawn a cleanup future whose completion guard was already built. Callers
/// that must hand the guard to an outer task use this so an unpolled outer
/// future still settles every receiver it owns.
pub(crate) fn spawn_completion<T: Send + Sync + 'static, E: Send + Sync + 'static>(
    executor: &Handle,
    completion: Completion<T, E>,
    cleanup: impl Future<Output = Result<T, E>> + Send + 'static,
) {
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        executor.spawn(async move { completion.finish(cleanup.await) })
    }));
}

/// Borrow a string panic payload; callers retain their own fallback wording.
/// Borrowing also leaves payload destruction at the caller's cleanup boundary.
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> Option<&str> {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
}

/// Recover the same guard after poisoning without clearing the poison flag.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_messages_preserve_string_bytes_and_leave_fallback_to_callers() {
        for message in ["", "panic", "错误\0\nſK"] {
            assert_eq!(panic_message(&message), Some(message), "borrowed payload");
            assert_eq!(
                panic_message(&message.to_owned()),
                Some(message),
                "owned payload"
            );
        }
        assert_eq!(panic_message(&42_u8), None, "opaque payload");
    }

    #[test]
    fn recovering_lock_preserves_value_mutability_and_poison() {
        let mutex = Mutex::new(1);
        *lock(&mutex) += 1;
        assert_eq!(*lock(&mutex), 2);
        assert!(!mutex.is_poisoned());
        let panic = std::panic::catch_unwind(|| {
            let mut value = mutex.lock().unwrap();
            *value = 7;
            panic!("poison the mutex");
        });
        assert!(panic.is_err());
        assert!(mutex.is_poisoned());
        *lock(&mutex) += 1;
        assert_eq!(*lock(&mutex), 8, "poisoned value retained");
        assert!(mutex.is_poisoned(), "recovery must not clear poisoning");
    }
}
