//! Port of TS `src/always-on/runtime/SignalWatcher.ts` — the dormancy
//! wake-up signal.
//!
//! The port's one formal deviation (documented in
//! `A7.8-ALWAYS-ON-BACKGROUND-001-R1.md`): the TS watcher wraps
//! `fs.watch({ recursive: true })`, which the Node docs and the TS source
//! both note attaches to the root only on Linux. The Rust standard library
//! has no recursive watch, so this port polls the project root's direct
//! entries every `poll_interval_ms` (1000) against a snapshot taken at
//! `start` — root-only coverage, matching the Linux baseline the golden
//! records were captured on (`d1`). The debounce contract is preserved: a
//! qualifying change (re)arms a `debounce_ms` deadline measured from the last
//! qualifying change (TS `handleEvent` clears and re-arms its timer), and
//! `on_signal` fires once when the deadline passes with no further change.
//! The deadline lands on a poll boundary, so it fires within
//! `poll_interval_ms` after the TS-exact moment.
//!
//! The TS `baselineAt` option is dead in the baseline (constructed, never
//! read); this port needs no equivalent beyond the start snapshot.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use tokio::{
    sync::Notify,
    time::{Duration, Instant, sleep},
};

/// `DEFAULT_IGNORE_GLOBS` (parseAlwaysOnConfig.ts:28-35).
pub(crate) const DEFAULT_IGNORE_GLOBS: &[&str] = &[
    "**/.git/**",
    "**/node_modules/**",
    "**/.pilotdeck/**",
    "**/.pilotdeck-always-on/**",
    "**/dist/**",
    "**/.DS_Store",
];

/// `SignalWatcherOptions` (SignalWatcher.ts:5-15) plus the poll interval the
/// deviation adds. `on_error` receives the message the TS `Error` would carry
/// (`projectRoot not found: …` / the fs error).
pub(crate) struct SignalWatcherOptions {
    pub project_root: PathBuf,
    pub ignore_globs: Vec<String>,
    pub debounce_ms: u64,
    pub on_signal: Arc<dyn Fn() + Send + Sync>,
    pub on_error: Arc<dyn Fn(&str) + Send + Sync>,
    pub poll_interval_ms: u64,
}

impl SignalWatcherOptions {
    /// The scheduler's shape (DiscoveryScheduler.ts:175-189): the default
    /// ignore globs, the 2000ms debounce, and this port's 1000ms poll.
    pub(crate) fn with_defaults(
        project_root: PathBuf,
        on_signal: Arc<dyn Fn() + Send + Sync>,
        on_error: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        Self {
            project_root,
            ignore_globs: DEFAULT_IGNORE_GLOBS
                .iter()
                .map(|glob| (*glob).into())
                .collect(),
            debounce_ms: 2000,
            on_signal,
            on_error,
            poll_interval_ms: 1000,
        }
    }
}

/// `compileGlob` (SignalWatcher.ts:107-147) on the regex crate: `**/` is any
/// leading segment(s), `*` is any chars but `/`, `?` one non-`/` char, and
/// the regex specials escape. `/` passes through unescaped — the regex crate
/// rejects `\/`, the one translation difference from JS (where `\/` is just
/// `/`). The translation is otherwise the TS one verbatim: the trailing
/// `/**` of `**/.git/**` still requires the `/` after the literal, so
/// `.gitignore` and `foo/.git` do NOT match (Node v22.22.0, measured).
fn compile_glob(glob: &str) -> regex::Regex {
    let chars: Vec<char> = glob.chars().collect();
    let mut body = String::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '*' {
            if chars.get(index + 1) == Some(&'*') {
                if chars.get(index + 2) == Some(&'/') {
                    body += "(?:.*/)?";
                    index += 3;
                    continue;
                }
                body += ".*";
                index += 2;
                continue;
            }
            body += "[^/]*";
            index += 1;
            continue;
        }
        if ch == '?' {
            body += "[^/]";
            index += 1;
            continue;
        }
        if matches!(
            ch,
            '.' | '+' | '^' | '$' | '|' | '(' | ')' | '\\' | '[' | ']' | '{' | '}'
        ) {
            body.push('\\');
        }
        body.push(ch);
        index += 1;
    }
    regex::Regex::new(&format!("^(?:{body})$")).expect("#2294 glob compiles")
}

/// One root-level entry's identity: (mtime, size, is_dir).
type EntryStat = (SystemTime, u64, bool);

struct WatcherInner {
    options: SignalWatcherOptions,
    globs: Vec<regex::Regex>,
    stopped: AtomicBool,
    started: AtomicBool,
    notify: Notify,
    snapshot: std::sync::Mutex<HashMap<String, EntryStat>>,
    pending: std::sync::Mutex<Option<Instant>>,
}

/// `SignalWatcher` (SignalWatcher.ts:28-97). `stop` never joins: the poll
/// task exits on its own once the flag is set, so a callback may dispose the
/// watcher from inside `on_signal` — exactly what the scheduler's
/// `handleSignal` does — without deadlocking.
pub(crate) struct SignalWatcher {
    inner: Arc<WatcherInner>,
}

impl SignalWatcher {
    pub(crate) fn new(options: SignalWatcherOptions) -> Self {
        let globs = options
            .ignore_globs
            .iter()
            .map(|glob| compile_glob(glob))
            .collect();
        Self {
            inner: Arc::new(WatcherInner {
                options,
                globs,
                stopped: AtomicBool::new(false),
                started: AtomicBool::new(false),
                notify: Notify::new(),
                snapshot: std::sync::Mutex::new(HashMap::new()),
                pending: std::sync::Mutex::new(None),
            }),
        }
    }

    /// `start` (SignalWatcher.ts:38-57): a missing root reports through
    /// `on_error` and stays stopped; a root that cannot be scanned does the
    /// same, like the `watchFn` throw the TS catches.
    pub(crate) fn start(&self) {
        if self.inner.stopped.load(Ordering::SeqCst)
            || self.inner.started.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let root = &self.inner.options.project_root;
        if !root.exists() {
            (self.inner.options.on_error)(&format!("projectRoot not found: {}", root.display()));
            self.inner.stopped.store(true, Ordering::SeqCst);
            return;
        }
        match scan(root) {
            Ok(snapshot) => {
                *self
                    .inner
                    .snapshot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = snapshot;
            }
            Err(message) => {
                (self.inner.options.on_error)(&message);
                self.inner.stopped.store(true, Ordering::SeqCst);
                return;
            }
        }
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let interval = Duration::from_millis(inner.options.poll_interval_ms.max(1));
            loop {
                tokio::select! {
                    _ = sleep(interval) => {}
                    _ = inner.notify.notified() => {}
                }
                if inner.stopped.load(Ordering::SeqCst) {
                    break;
                }
                if let Err(message) = poll_once(&inner) {
                    (inner.options.on_error)(&message);
                    break;
                }
                let deadline = *inner.pending.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(deadline) = deadline
                    && Instant::now() >= deadline
                {
                    *inner.pending.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    if !inner.stopped.load(Ordering::SeqCst) {
                        (inner.options.on_signal)();
                    }
                }
            }
        });
    }

    /// `stop` (SignalWatcher.ts:59-73): set the flag, drop a pending
    /// debounce, wake the poll task.
    pub(crate) fn stop(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        *self.inner.pending.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.inner.notify.notify_one();
    }

    /// `handleEvent` (SignalWatcher.ts:76-86), public for tests: an ignored
    /// or post-stop event is a no-op; anything else (re)arms the debounce.
    pub(crate) fn handle_event(&self, filename: &str) {
        if self.inner.stopped.load(Ordering::SeqCst) {
            return;
        }
        if self.should_ignore(filename) {
            return;
        }
        let deadline =
            Instant::now() + Duration::from_millis(self.inner.options.debounce_ms.max(1));
        *self.inner.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(deadline);
    }

    /// `shouldIgnore` (SignalWatcher.ts:88-96). The port's events arrive as
    /// root-relative names already, so the `relative()` step is the identity
    /// the TS intends.
    pub(crate) fn should_ignore(&self, filename: &str) -> bool {
        if filename.is_empty() {
            return true;
        }
        self.inner.globs.iter().any(|glob| glob.is_match(filename))
    }
}

/// One poll: diff the current entries against the snapshot, feed every
/// created/changed/deleted entry through `handle_event`, replace the
/// snapshot. A root that cannot be read is the error that stops the watcher.
fn poll_once(inner: &Arc<WatcherInner>) -> Result<(), String> {
    let entries = scan(&inner.options.project_root)?;
    let mut changed: Vec<(String, bool)> = Vec::new();
    {
        let mut snapshot = inner.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        for (name, (mtime, size, is_dir)) in &entries {
            if snapshot.get(name) != Some(&(*mtime, *size, *is_dir)) {
                changed.push((name.clone(), *is_dir));
            }
        }
        for (name, (_, _, is_dir)) in snapshot.iter() {
            if !entries.contains_key(name) {
                changed.push((name.to_string(), *is_dir));
            }
        }
        *snapshot = entries;
    }
    let watcher = SignalWatcher {
        inner: inner.clone(),
    };
    for (name, is_dir) in &changed {
        // A changed directory entry is this port's spelling of "something
        // inside it changed" — the TS `fs.watch` event carries the inner
        // path. Ignore globs like `**/.git/**` key on the `/` after the
        // literal, so the directory is matched with its trailing slash;
        // `**/.git/**` then covers `.git/` the way it covers `.git/config`.
        let spelling = if *is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        watcher.handle_event(&spelling);
    }
    Ok(())
}

fn scan(root: &Path) -> Result<HashMap<String, EntryStat>, String> {
    let mut entries = HashMap::new();
    let dir = std::fs::read_dir(root).map_err(|error| error.to_string())?;
    for entry in dir {
        // An entry that vanishes mid-scan (create/delete race) simply drops
        // out of this poll; it counts as a change when it reappears.
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        entries.insert(
            name,
            (
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                metadata.len(),
                metadata.is_dir(),
            ),
        );
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use std::sync::Mutex;

    fn scratch(tag: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("always-on-2294-watcher-{tag}"));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("#2294 mkdir");
        home
    }

    // `compileGlob` (SignalWatcher.ts:107-147) — the translation and its
    // quirks, which the port inherits verbatim from the regex the TS builds.
    #[test]
    fn compiles_the_default_globs_with_the_ts_matches() {
        for (glob, candidate, expected) in [
            ("**/.git/**", ".git/config", true),
            ("**/.git/**", "a/.git/x", true),
            // The trailing `**` is `.*` after the literal `/`, so a file
            // merely named like `.gitignore` is not swallowed (measured
            // against the TS compileGlob on Node v22.22.0).
            ("**/.git/**", "foo/.gitignore", false),
            ("**/.git/**", "x/git/y", false),
            ("**/.DS_Store", ".DS_Store", true),
            ("**/.DS_Store", "a/b/.DS_Store", true),
            ("**/.DS_Store", ".DS_Storex", false),
            ("**/node_modules/**", "node_modules/pkg", true),
            ("**/node_modules/**", "a/node_modules/x/file", true),
            ("**/dist/**", "dist/out.js", true),
            // The leading `**/` is optional, so a root-level name matches
            // (Node v22.22.0, measured).
            ("**/pilotdeck", "pilotdeck", true),
            ("**/.pilotdeck/**", ".pilotdeck/state.json", true),
            ("*", "plain.txt", true),
            ("*", "sub/plain.txt", false),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("a.c", "axc", false),
        ] {
            let regex = compile_glob(glob);
            negative_assert_eq!(
                regex.is_match(candidate),
                expected,
                "#2294 {glob} vs {candidate}"
            );
        }
    }

    fn watcher(
        root: &Path,
        signals: Arc<Mutex<usize>>,
        errors: Arc<Mutex<Vec<String>>>,
    ) -> SignalWatcher {
        let signal_root = signals.clone();
        SignalWatcher::new(SignalWatcherOptions {
            project_root: root.to_path_buf(),
            ignore_globs: DEFAULT_IGNORE_GLOBS.iter().map(|g| (*g).into()).collect(),
            debounce_ms: 200,
            on_signal: Arc::new(move || {
                *signal_root.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            }),
            on_error: Arc::new(move |message: &str| {
                errors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(message.into());
            }),
            poll_interval_ms: 50,
        })
    }

    // A qualifying change fires exactly one signal after the debounce, and a
    // burst within the window slides into the same one signal (the TS
    // clear-and-re-arm).
    #[tokio::test(start_paused = true)]
    async fn a_settled_change_fires_one_signal() {
        let root = scratch("signal");
        let signals = Arc::new(Mutex::new(0));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let watcher = watcher(&root, signals.clone(), errors.clone());
        watcher.start();
        // Burst one: create then grow the file inside the debounce window.
        std::fs::write(root.join("signal.txt"), "x").expect("#2294 write");
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::fs::write(root.join("signal.txt"), "xxxx").expect("#2294 write");
        // Past the second change's deadline: exactly one signal.
        tokio::time::sleep(Duration::from_millis(500)).await;
        negative_assert_eq!(
            *signals.lock().unwrap_or_else(|e| e.into_inner()),
            1,
            "#2294 a burst within the debounce is one signal"
        );
        // A settled later change is another signal.
        std::fs::write(root.join("signal.txt"), "later change with length").expect("#2294 write");
        tokio::time::sleep(Duration::from_millis(500)).await;
        negative_assert_eq!(
            *signals.lock().unwrap_or_else(|e| e.into_inner()),
            2,
            "#2294 a later settled change fires again"
        );
        negative_assert_eq!(
            errors.lock().unwrap_or_else(|e| e.into_inner()).len(),
            0,
            "#2294 no watcher errors"
        );
        watcher.stop();
    }

    // `stop` (SignalWatcher.ts:59-73): the pending debounce dies with it.
    #[tokio::test(start_paused = true)]
    async fn stop_kills_the_pending_debounce() {
        let root = scratch("stop");
        let signals = Arc::new(Mutex::new(0));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let watcher = watcher(&root, signals.clone(), errors.clone());
        watcher.start();
        std::fs::write(root.join("signal.txt"), "x").expect("#2294 write");
        tokio::time::sleep(Duration::from_millis(50)).await;
        watcher.stop();
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::fs::write(root.join("signal.txt"), "post stop").expect("#2294 write");
        tokio::time::sleep(Duration::from_millis(500)).await;
        negative_assert_eq!(
            *signals.lock().unwrap_or_else(|e| e.into_inner()),
            0,
            "#2294 no signal after stop"
        );
    }

    // An ignored path never arms the debounce (`shouldIgnore`,
    // SignalWatcher.ts:88-96) — the always-on state directory itself, for
    // instance, must not self-excite.
    #[tokio::test(start_paused = true)]
    async fn ignored_paths_never_signal() {
        let root = scratch("ignored");
        let signals = Arc::new(Mutex::new(0));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let watcher = watcher(&root, signals.clone(), errors.clone());
        watcher.start();
        std::fs::create_dir_all(root.join(".git")).expect("#2294 mkdir");
        std::fs::write(root.join(".git").join("config"), "[core]").expect("#2294 write");
        std::fs::write(root.join(".DS_Store"), "junk").expect("#2294 write");
        tokio::time::sleep(Duration::from_millis(600)).await;
        negative_assert_eq!(
            *signals.lock().unwrap_or_else(|e| e.into_inner()),
            0,
            "#2294 ignored entries never signal"
        );
        negative_assert_eq!(watcher.should_ignore(""), true, "#2294 empty filename");
        negative_assert_eq!(
            watcher.should_ignore("plain.txt"),
            false,
            "#2294 plain file"
        );
        watcher.stop();
    }

    // A missing root reports through `on_error` and stays stopped
    // (SignalWatcher.ts:40-44).
    #[tokio::test(start_paused = true)]
    async fn a_missing_root_errors_and_stops() {
        let root = scratch("missing").join("gone");
        let signals = Arc::new(Mutex::new(0));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let watcher = watcher(&root, signals.clone(), errors.clone());
        watcher.start();
        let errors = errors.lock().unwrap_or_else(|e| e.into_inner());
        negative_assert_eq!(errors.len(), 1, "#2294 one on_error");
        negative_assert_eq!(
            errors[0],
            format!("projectRoot not found: {}", root.display()),
            "#2294 the projectRoot-not-found message"
        );
        drop(errors);
        // A second start on the stopped watcher is a no-op.
        watcher.start();
        tokio::time::sleep(Duration::from_millis(200)).await;
        negative_assert_eq!(
            *signals.lock().unwrap_or_else(|e| e.into_inner()),
            0,
            "#2294 no signals from a stopped watcher"
        );
    }

    // `handle_event` is the pure debounce arm; ignored and post-stop events
    // are no-ops (SignalWatcher.ts:76-86).
    #[tokio::test(start_paused = true)]
    async fn handle_event_arms_the_debounce() {
        let root = scratch("handle-event");
        let signals = Arc::new(Mutex::new(0));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let watcher = watcher(&root, signals.clone(), errors.clone());
        watcher.handle_event("plain.txt");
        negative_assert_eq!(
            watcher
                .inner
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some(),
            true,
            "#2294 a qualifying event arms the deadline"
        );
        watcher.handle_event(".git/config");
        watcher.handle_event("");
        tokio::time::sleep(Duration::from_millis(400)).await;
        negative_assert_eq!(
            *signals.lock().unwrap_or_else(|e| e.into_inner()),
            0,
            "#2294 an unstarted watcher never fires (no poll task)"
        );
        watcher.stop();
    }
}
