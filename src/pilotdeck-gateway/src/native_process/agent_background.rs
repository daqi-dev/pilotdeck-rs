//! Session-owned fork generation. Detached task ownership extends this same owner.
use super::*;
use crate::runtime::{
    project::{RuntimeLease, RuntimeResult},
    refresh::BoxFuture,
};
use std::sync::atomic::AtomicBool;

pub(crate) struct NativeForkOwner {
    generation: Mutex<Option<Arc<RuntimeLease>>>,
    closed: AtomicBool,
    releasing: Mutex<bool>,
    completion: watch::Sender<Option<RuntimeResult>>,
    executor: tokio::runtime::Handle,
}
impl NativeForkOwner {
    pub(in crate::native_process) fn new(lease: RuntimeLease) -> Arc<Self> {
        Arc::new(Self {
            generation: Mutex::new(Some(Arc::new(lease))),
            closed: AtomicBool::new(false),
            releasing: Mutex::new(false),
            completion: watch::channel(None).0,
            executor: tokio::runtime::Handle::current(),
        })
    }
    pub(in crate::native_process) fn generation(&self) -> Option<Arc<RuntimeLease>> {
        let generation = lock(&self.generation);
        if self.closed.load(Ordering::SeqCst) {
            None
        } else {
            generation.clone()
        }
    }
    pub(in crate::native_process) fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }
    pub(in crate::native_process) fn close_admission(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
    pub(in crate::native_process) fn release_generation(
        self: Arc<Self>,
    ) -> BoxFuture<RuntimeResult> {
        self.close_admission();
        let mut completion = self.completion.subscribe();
        let first = {
            let mut releasing = lock(&self.releasing);
            let first = !*releasing;
            *releasing = true;
            first
        };
        if first {
            let owner = self.clone();
            crate::runtime::cleanup_task::spawn_with_error(
                &self.executor,
                self.completion.clone(),
                RuntimeError::new("Native fork generation release was dropped or panicked."),
                async move {
                    let generation = lock(&owner.generation).take();
                    match generation {
                        Some(generation) => match Arc::try_unwrap(generation) {
                            Ok(mut lease) => lease.release().await,
                            Err(mut generation) => loop {
                                // Do not detach a still-active child or pretend its
                                // execution world is released. The enclosing drain
                                // retains this future and reports its own deadline.
                                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                                match Arc::try_unwrap(generation) {
                                    Ok(mut lease) => break lease.release().await,
                                    Err(held) => generation = held,
                                }
                            },
                        },
                        None => Ok(()),
                    }
                },
            );
        }
        Box::pin(async move {
            loop {
                if let Some(result) = completion.borrow_and_update().clone() {
                    return result;
                }
                completion
                    .changed()
                    .await
                    .map_err(|_| RuntimeError::new("Native fork generation completion lost."))?;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle, RuntimeState};
    #[tokio::test]
    async fn fork_stays_in_retired_generation_and_release_waits_for_live_borrow() {
        let registry = ProjectRuntimeRegistry::new(
            "/fixture-native-generation",
            Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
        );
        let lease = registry.acquire(None).unwrap();
        let old = lease.runtime().clone();
        let owner = NativeForkOwner::new(lease);
        let held = owner.generation().unwrap();
        registry.reload().await.unwrap();
        let current = registry.acquire(None).unwrap();
        assert!(!Arc::ptr_eq(&old, current.runtime()));
        assert_eq!(old.state(), RuntimeState::Retired);
        assert!(old.acquire().is_err());
        let mut derivative = held.fork().unwrap();
        assert!(Arc::ptr_eq(derivative.runtime(), &old));
        assert_eq!(old.lease_count(), 2);
        derivative.release().await.unwrap();
        assert_eq!(old.lease_count(), 1);
        let mut closing = owner.clone().release_generation();
        assert!(!owner.is_open());
        assert!(owner.generation().is_none());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut closing)
                .await
                .is_err()
        );
        drop(held);
        tokio::time::timeout(std::time::Duration::from_secs(2), closing)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.state(), RuntimeState::Disposed);
        drop(current);
        registry.dispose().await.unwrap();
    }
}
