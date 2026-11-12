//! #1457 fix-r1: one write fence per transcript file for the paths that append
//! a metadata entry (`session_rename_or_tag`, `session_record_status` and
//! `record_model_selection`) and, since #1860, for the session factory's own
//! hydration.
//!
//! Each of them either numbers its own append from the tail it just read
//! (`transcript_adapter.rs`: `inner.sequence + 1`) or appends through a live
//! recorder. The stores do not share any cross-instance mutex, so a fresh
//! writer that restores the tail concurrently with a live one makes both claim
//! `N+1`; the next `restore()` rejects the file with
//! `sequence_not_increasing` and the session can neither be continued nor
//! resumed. Serializing the whole read-modify-write per transcript keeps the
//! chain strictly increasing.
//!
//! fix-r3 adds the second half: the fence only excludes requests that use it,
//! so a cold append also takes the session writer lease
//! ([`crate::runtime::session::factory::try_writer_lease`]) that a live or
//! hydrating session holds for its lifetime. That is what keeps a cold append
//! from interleaving with `factory`'s `restore()` → publish-recorder window,
//! and it is why the live path can be sure it is looking at the only writer.
//!
//! #1860 adds the missing holder: hydration itself. `factory::construct` reads
//! the tail into the recorder's in-memory sequence and only then publishes the
//! recorder, so the two sides of that window must be fenced like any other
//! read-modify-write. `initialized` takes this fence after the writer lease and
//! holds it until `prepared.host` has published the recorder; the lease keeps
//! *other processes and early requests* out, the fence keeps the in-process
//! "who writes this tail" decision atomic with its append.
//!
//! Keys are the canonical transcript path, not the request spelling. That is
//! the identity the writer lease ([`crate::runtime::session::factory::
//! WriterLease`] and its `LedgerLock`) already uses, so one file cannot end up
//! with two fences; and the path itself is derived in one place
//! ([`session_transcript_path`]) that `native_process::assembly` also uses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use pilotdeck_protocol::JsString;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::runtime::connection::RequestError;
use crate::runtime::session::factory::WriterLease;

/// Weak entries only: a fence lives as long as some request is holding or
/// waiting on it, and dead handles are pruned on every acquisition.
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The transcript file one session id maps to under one project.
///
/// This is the single derivation of that path: the fence keys on it and
/// `native_process::assembly::transcript_path` builds storage paths from it, so
/// a change here can never leave the fence guarding a file the store does not
/// open. The chat directory comes from the project marker lookup and the file
/// name from `sanitize_session_id` (which folds `/` and `\` to `-`), so `/p`
/// and `/p/.` are one directory and `a/b` / `a-b` are one file.
pub(crate) fn session_transcript_path(
    home: &Path,
    project_root: &Path,
    session_id: &JsString,
) -> PathBuf {
    crate::session_chat_dir(home, project_root, &project_root.to_string_lossy()).join(format!(
        "{}.jsonl",
        crate::storage::sanitize_session_id(session_id)
    ))
}

/// The fence key. Canonicalizing here is what makes it the same identity the
/// writer lease and the ledger lock use (`try_writer_lease`,
/// `LedgerLock::acquire`), so a metadata request and the hydrating session can
/// never fence two names of one file.
///
/// A resolution failure is an error, not a fallback to the supplied spelling:
/// keying on the raw path would let one file be fenced under two names (an
/// unresolved spelling now, the canonical one once the path appears), which is
/// the double-writer this fence exists to prevent. Callers surface the error;
/// a path that cannot be resolved is one the append could not open either.
fn fence_key(transcript_path: &Path) -> Result<PathBuf, RequestError> {
    crate::storage::canonical_transcript_path(transcript_path)
        .map_err(|error| RequestError::new(format!("Session transcript cannot be fenced: {error}")))
}

/// The session writer lease for a transcript no session owns right now.
///
/// `Ok(None)` means a session holds it — hydrating (its recorder is not
/// published yet) or closing (its writer is already closed) — so a cold
/// append would fork the sequence chain. Callers must then append through the
/// recorder or surface the retryable busy error; they must never write cold
/// anyway. A storage failure on the way to the lease is an `Err` (#1873): the
/// pre-fix shape folded it into the busy answer and misreported a broken
/// transcript directory as a session that would eventually drain.
pub(crate) fn cold_write_lease(
    transcript_path: &Path,
) -> Result<Option<WriterLease>, RequestError> {
    crate::runtime::session::factory::try_writer_lease(transcript_path)
        .map_err(|error| RequestError::new(error.to_string()))
}

/// The retryable error a metadata request reports when a session owns the
/// transcript but has no published recorder yet (hydrating or closing). The
/// same code the session factory uses when a writer is still draining.
pub(crate) fn writer_busy_error() -> crate::runtime::connection::RequestError {
    crate::runtime::connection::RequestError {
        code: "SESSION_WRITER_BUSY".into(),
        message: "Session writer is still draining; retry session recreation.".into(),
        details: None,
        validation: None,
    }
}

/// The three-segment cold-write protocol every metadata entry goes through
/// (`#1874` consolidation of the config, rename and status callers):
///
/// 1. resolve the live recorder for the folded transcript and append through
///    it — a loaded session owns the writer (and the lease) with an in-memory
///    sequence, so the entry continues its chain (`#1457` fix-r2);
/// 2. otherwise take the writer lease a session holds and append cold, with
///    the tail restored first so the entry continues the chain (fix-r1/r3);
/// 3. a busy lease means a session owns the transcript after all: re-query
///    the recorder (appending through it when it is published) and otherwise
///    surface the retryable busy error — never write a second chain beside it.
///
/// `live` is the same closure for the first lookup and the re-query, so one
/// checked append covers both paths for its caller. It returns the key the
/// session is registered under, so the caller can close *that* session. The
/// caller-supplied closures keep this module free of the `native_process`
/// recorder map (`runtime` must not import it); `cold` appends the entry
/// through the storage this helper restored.
pub(crate) fn append_metadata_with_lease(
    home: &Path,
    project_root: &Path,
    session_key: &JsString,
    live: impl Fn() -> Result<Option<JsString>, RequestError>,
    cold: impl FnOnce(&crate::storage::SessionStorage) -> Result<(), RequestError>,
) -> Result<Option<JsString>, RequestError> {
    if let Some(owner) = live()? {
        return Ok(Some(owner));
    }
    let home2 = home.to_path_buf();
    let adapter = crate::storage::AsyncTranscriptAdapter::new(
        Arc::new(crate::storage::JsonlTranscriptStore::new()),
        Arc::new(move |k: &crate::storage::ProjectKey| {
            crate::session_chat_dir(&home2, &k.project_root, &k.project_root.to_string_lossy())
        }),
    );
    let storage = adapter
        .create_session_storage(crate::storage::SessionStorageInput {
            project_root: project_root.to_path_buf(),
            pilot_home: home.to_path_buf(),
            session_id: session_key.clone(),
            now: None,
        })
        .map_err(|e| RequestError::new(e.to_string()))?;
    let Some(_lease) = cold_write_lease(&storage.transcript_path)? else {
        // A session claimed the transcript between the lookup and here.
        // Append through it when it is published; otherwise say busy —
        // never write a second chain beside it.
        return match live()? {
            Some(owner) => Ok(Some(owner)),
            None => Err(writer_busy_error()),
        };
    };
    // Seed the writer's sequence from the existing transcript so the entry
    // continues the chain rather than restarting at 1. A probe error
    // (`Some(Err(_))` / `None`) currently skips the restore and can brick a
    // stale transcript with a sequence:1 rewrite — registered in #2686.
    if matches!(storage.transcript_exists(), Some(Ok(true))) {
        storage
            .restore()
            .map_err(|e| RequestError::new(e.to_string()))?;
    }
    cold(&storage)?;
    Ok(None)
}

/// Hold the returned guard across the whole read-modify-write of one transcript
/// file. Two writers for the same file run one after the other; a writer for
/// any other transcript is unaffected.
pub(crate) async fn acquire(transcript_path: &Path) -> Result<OwnedMutexGuard<()>, RequestError> {
    // Canonicalization touches the filesystem (it creates the chat directory and
    // resolves a symlinked leaf), so it stays off the async worker.
    let supplied = transcript_path.to_path_buf();
    let key = tokio::task::spawn_blocking(move || fence_key(&supplied))
        .await
        .map_err(|error| {
            RequestError::new(format!("Session transcript fence task failed: {error}"))
        })??;
    Ok(acquire_key(key).await)
}

/// Take the fence for an already-resolved key.
///
/// Split out from [`acquire`] so a waiter can be placed *at* the fence without
/// first spending an unbounded amount of time resolving the path: a test that
/// only checks "the waiter has not finished yet" would otherwise pass on a slow
/// resolution alone and let a dropped guard survive.
pub(crate) async fn acquire_key(key: PathBuf) -> OwnedMutexGuard<()> {
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

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pdrs-1860-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("#1860 scratch");
        root
    }

    /// Two acquirers for one transcript never overlap, and a different
    /// transcript is free.
    #[tokio::test]
    async fn one_fence_serializes_and_other_transcripts_do_not() {
        let root = scratch("serialize");
        let path = root.join("session-1.jsonl");
        let first = acquire(&path).await.expect("#1860 first fence");
        let other_root = root.clone();
        let second = tokio::spawn(async move {
            let _guard = acquire(&other_root.join("session-1.jsonl"))
                .await
                .expect("#1860 second fence");
            true
        });
        // The second acquirer must still be waiting while the first holds it.
        tokio::task::yield_now().await;
        assert!(!second.is_finished(), "same file must not run concurrently");
        let other = tokio::time::timeout(std::time::Duration::from_millis(250), async {
            acquire(&root.join("session-2.jsonl"))
                .await
                .expect("#1860 other fence")
        })
        .await;
        assert!(other.is_ok(), "a different transcript must not be blocked");
        drop(first);
        assert!(second.await.unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A released fence is immediately reusable: the registry entry a dropped
    /// guard leaves behind is a `Weak`, so the next acquirer rebuilds it
    /// instead of deadlocking on a detached lock.
    #[tokio::test]
    async fn released_fence_is_immediately_reusable() {
        let root = scratch("reuse");
        let path = root.join("ephemeral.jsonl");
        drop(acquire(&path).await.expect("#1860 first acquire"));
        let again = tokio::time::timeout(std::time::Duration::from_millis(250), async {
            acquire(&path).await.expect("#1860 second acquire")
        })
        .await;
        assert!(again.is_ok(), "a released fence must not stay locked");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A transcript whose canonical identity cannot be resolved is refused, and
    /// the rejected spelling is not left fenced. A lexical fallback key would
    /// guard one file under two names — an unresolved spelling now, the
    /// canonical one once the path appears — which is the double-writer this
    /// fence exists to prevent.
    #[tokio::test]
    async fn unresolvable_transcript_is_refused_not_keyed_on_its_spelling() {
        let root = scratch("unresolved");
        // A dangling leaf symlink: `lstat` succeeds, `canonicalize` fails.
        let dangling = root.join("dangling.jsonl");
        std::os::unix::fs::symlink(root.join("missing.jsonl"), &dangling)
            .expect("#1860 dangling leaf");
        assert!(
            acquire(&dangling).await.is_err(),
            "#1860 an unresolvable transcript must not be fenced"
        );
        // Nothing was registered under the spelling, so it is free right away.
        let free = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            acquire_key(dangling.clone()),
        )
        .await;
        assert!(
            free.is_ok(),
            "#1860 the refused spelling must not stay locked"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The key is the canonical transcript identity, so two spellings of one
    /// file take the same fence while two files keep their own. `.` / `..`
    /// handling and a symlinked leaf are exactly what a lexical key missed: it
    /// handed one file two fences, and the read-modify-write they were supposed
    /// to serialize could interleave.
    #[test]
    fn fence_keys_follow_the_canonical_transcript_path() {
        let root = scratch("keys");
        let dir = root.join("project");
        std::fs::create_dir_all(&dir).expect("#1860 project dir");
        let real = dir.join("session-1.jsonl");
        std::fs::write(&real, b"").expect("#1860 transcript");
        let alias = root.join("alias.jsonl");
        std::os::unix::fs::symlink(&real, &alias).expect("#1860 symlink");
        assert_eq!(
            fence_key(&real).expect("#1860 real key"),
            fence_key(&alias).expect("#1860 alias key"),
            "a symlinked leaf is the same transcript"
        );
        assert_eq!(
            fence_key(&real).expect("#1860 same transcript"),
            fence_key(&dir.join(".").join("session-1.jsonl")).expect("#1860 dot key"),
            "a `.` component is the same transcript"
        );
        assert_eq!(
            fence_key(&real).expect("#1860 same transcript"),
            fence_key(&dir.join("..").join("project").join("session-1.jsonl"))
                .expect("#1860 dotdot key"),
            "a `..` round trip is the same transcript"
        );
        assert_ne!(
            fence_key(&real).expect("#1860 same transcript"),
            fence_key(&dir.join("session-2.jsonl")).expect("#1860 other key"),
            "distinct transcripts must stay distinct fences"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The shared derivation folds the spellings the store folds: `/p` and
    /// `/p/.` are one chat directory and `a/b` / `a-b` are one file name, while
    /// distinct sessions stay distinct.
    #[test]
    fn session_transcript_path_follows_the_stored_identity() {
        let home = Path::new("/tmp/pdrs-1860-home");
        let project = Path::new("/project");
        let file = session_transcript_path(home, project, &"a/b".into());
        assert_eq!(
            file,
            session_transcript_path(home, project, &"a-b".into()),
            "the store folds `/` in the session id, so the file is the same"
        );
        assert_eq!(
            file,
            session_transcript_path(home, Path::new("/project/."), &"a/b".into()),
            "a trailing `/.` is the same chat directory"
        );
        assert_eq!(
            file,
            session_transcript_path(home, Path::new("/project/../project"), &"a/b".into()),
            "a `..` round trip is the same chat directory"
        );
        assert_ne!(
            file,
            session_transcript_path(home, project, &"a-c".into()),
            "distinct sessions must stay distinct files"
        );
    }

    /// #1623 (方案 B residual): the stored identity stays lossy — a lone
    /// surrogate and an explicit U+FFFD are still one file, because
    /// `sanitize_session_id` renders through `String::from_utf16_lossy`. The
    /// human decision kept this alias (no sanitize injectivity change, no
    /// migration); the `require_session_key` entry guards are the defense.
    /// This pins the residual, so a future change to the derivation shows up
    /// against the registered divergence
    /// (`A7-W5-SESSION-KEY-SURROGATE-REJECT-001`).
    #[test]
    fn session_transcript_path_keeps_the_surrogate_fffd_alias() {
        let home = Path::new("/tmp/pdrs-1623-home");
        let project = Path::new("/project");
        let lone = session_transcript_path(
            home,
            project,
            &pilotdeck_protocol::JsString::from_utf16(vec![0x73, 0xd800]),
        );
        let repl = session_transcript_path(home, project, &"s\u{fffd}".into());
        assert_eq!(
            lone, repl,
            "#1623 the lossy stored identity keeps the surrogate/FFFD alias (方案 B residual)"
        );
    }
}
