//! #1457 fix-r1: one write lock per (project root, session key) for the
//! transcript paths that build a *fresh* writer, restore the tail and append a
//! metadata entry (`session_rename_or_tag` and `record_model_selection`).
//!
//! Each of them numbers its own append from the tail it just read
//! (`transcript_adapter.rs`: `inner.sequence + 1`), and the stores do not share
//! any cross-instance mutex. Two concurrent requests for one session therefore
//! both read `N` and both write `N+1`; the next `restore()` rejects the file
//! with `sequence_not_increasing` and the session can neither be continued nor
//! resumed. Serializing the whole read-modify-write per key keeps the chain
//! strictly increasing.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use pilotdeck_protocol::JsString;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// Weak entries only: a lock lives as long as some request is holding or
/// waiting on it, and dead handles are pruned on every acquisition.
static LOCKS: LazyLock<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn registry_key(project_root: &str, session_key: &JsString) -> String {
    // Key on the transcript the store would open, not on the request spelling:
    // the chat directory comes from a lexically resolved project path and the
    // file name from `sanitize_session_id` (which folds `/` and `\` to `-`).
    // `/p` and `/p/.` are one transcript, and so are `a/b` and `a-b`; keying
    // on the raw request would hand each alias its own lock over one file.
    // Sanitizing also folds lone surrogates to U+FFFD, which can only merge two
    // distinct keys onto one lock -- more serialization, never less.
    let project = crate::metadata::resolve_path(
        std::path::Path::new(project_root),
        std::path::Path::new(project_root),
    );
    let project = project.to_string_lossy();
    let mut key = String::with_capacity(project.len() + 1 + session_key.units().len() * 2);
    key.push_str(&project);
    key.push('\u{0}');
    key.push_str(&crate::storage::sanitize_session_id(session_key));
    key
}

/// Hold the returned guard across the whole read-modify-write of one session's
/// transcript. Two requests for the same session run one after the other.
pub(super) async fn acquire(project_root: &str, session_key: &JsString) -> OwnedMutexGuard<()> {
    let key = registry_key(project_root, session_key);
    let lock = {
        let mut locks = LOCKS.lock().unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, entry| entry.strong_count() > 0);
        match locks.get(&key).and_then(Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(AsyncMutex::new(()));
                locks.insert(key, Arc::downgrade(&lock));
                lock
            }
        }
    };
    lock.lock_owned().await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two acquirers for one key never overlap, and a different key is free.
    #[tokio::test]
    async fn one_key_serializes_and_other_keys_do_not() {
        let key: JsString = "session-1".into();
        let first = acquire("/project", &key).await;
        let second = tokio::spawn(async move {
            let _guard = acquire("/project", &"session-1".into()).await;
            true
        });
        // The second acquirer must still be waiting while the first holds it.
        tokio::task::yield_now().await;
        assert!(!second.is_finished(), "same key must not run concurrently");
        let other = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            acquire("/project", &"session-2".into()),
        )
        .await;
        assert!(other.is_ok(), "a different session must not be blocked");
        drop(first);
        assert!(second.await.unwrap());
    }

    /// A released key is immediately reusable: the registry entry a dropped
    /// guard leaves behind is a `Weak`, so the next acquirer rebuilds it
    /// instead of deadlocking on a detached lock.
    #[tokio::test]
    async fn released_key_is_immediately_reusable() {
        let key: JsString = "ephemeral".into();
        drop(acquire("/project", &key).await);
        let again = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            acquire("/project", &key),
        )
        .await;
        assert!(again.is_ok(), "a released key must not stay locked");
    }

    /// Different projects and different sessions never share a lock, and the
    /// separator keeps a path/key pair from colliding with its neighbour.
    #[test]
    fn registry_keys_are_distinct_per_project_and_session() {
        let a = registry_key("/project-a", &"shared".into());
        let b = registry_key("/project-b", &"shared".into());
        assert_ne!(a, b, "the same session under two projects is two locks");
        let c = registry_key("/project-a", &"other".into());
        assert_ne!(a, c, "two sessions of one project are two locks");
        assert_ne!(
            registry_key("/a", &"b".into()),
            registry_key("/ab", &"".into()),
            "the separator must not be forgeable from the parts"
        );
    }

    /// The key is the on-disk transcript identity, so two spellings of one
    /// project and two session ids that sanitize to one file name take the same
    /// lock. Missing either alias lets two writers restore one tail and append
    /// the same `sequence + 1` (`sanitize_session_id` folds `/`; the store
    /// resolves the project lexically).
    #[test]
    fn registry_keys_follow_the_stored_transcript_identity() {
        assert_eq!(
            registry_key("/project", &"session-1".into()),
            registry_key("/project/.", &"session-1".into()),
            "a trailing `/.` is the same chat directory"
        );
        assert_eq!(
            registry_key("/project", &"session-1".into()),
            registry_key("/project/../project", &"session-1".into()),
            "a `..` round trip is the same chat directory"
        );
        assert_eq!(
            registry_key("/project", &"a/b".into()),
            registry_key("/project", &"a-b".into()),
            "the store folds `/` in the session id, so the file is the same"
        );
        assert_ne!(
            registry_key("/project", &"a/b".into()),
            registry_key("/project", &"a-c".into()),
            "distinct transcripts must stay distinct locks"
        );
    }
}
