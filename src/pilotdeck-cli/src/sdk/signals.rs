//! CLI-owned, independent once listeners for SIGINT and SIGTERM.
//!
//! `signal-hook` keeps its OS handler after unregistering the final action.
//! Keep a conditional default action installed instead: outside a turn, and
//! after the first delivery of either signal, its next delivery has the normal
//! OS behavior. All signal-handler work is confined to signal-hook's safe APIs.
use crate::Failure;
use futures_util::future::{Either, select};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag,
};
use std::{
    future::Future,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

struct Registration {
    interrupt: Arc<AtomicBool>,
    terminate: Arc<AtomicBool>,
    active: AtomicBool,
}

impl Registration {
    fn install() -> Result<Arc<Self>, String> {
        let registration = Arc::new(Self {
            interrupt: Arc::new(AtomicBool::new(true)),
            terminate: Arc::new(AtomicBool::new(true)),
            active: AtomicBool::new(false),
        });
        for (signal, seen) in [
            (SIGINT, &registration.interrupt),
            (SIGTERM, &registration.terminate),
        ] {
            // Registration order matters: the first delivery observes false,
            // then sets true; the second invokes the signal's default action.
            flag::register_conditional_default(signal, Arc::clone(seen))
                .map_err(|error| error.to_string())?;
            flag::register(signal, Arc::clone(seen)).map_err(|error| error.to_string())?;
        }
        Ok(registration)
    }
}

static REGISTRATION: OnceLock<Result<Arc<Registration>, String>> = OnceLock::new();

pub(super) struct Signals {
    registration: Arc<Registration>,
    observed: u8,
    removed: bool,
}

impl Signals {
    pub(super) fn install() -> Result<Self, Failure> {
        let registration = REGISTRATION
            .get_or_init(Registration::install)
            .as_ref()
            .map_err(|error| Failure::Error(format!("Cannot register CLI signals: {error}")))?;
        if registration.active.swap(true, Ordering::SeqCst) {
            return Err(Failure::Error(
                "CLI signal listeners are already active.".into(),
            ));
        }
        registration.interrupt.store(false, Ordering::SeqCst);
        registration.terminate.store(false, Ordering::SeqCst);
        let signals = Self {
            registration: Arc::clone(registration),
            observed: 0,
            removed: false,
        };
        observe("listeners-ready");
        Ok(signals)
    }

    /// Records deliveries without cancelling/restarting an initialization
    /// future. Abort shares the SDK's existing initialization promise.
    pub(super) async fn during<F: Future>(&mut self, future: F) -> F::Output {
        let mut future = Box::pin(future);
        loop {
            let selected = match select(future, Box::pin(self.next())).await {
                Either::Left((result, waiting_signal)) => {
                    drop(waiting_signal);
                    Ok(result)
                }
                Either::Right(((), pending_future)) => Err(pending_future),
            };
            match selected {
                Ok(result) => {
                    self.record_pending();
                    return result;
                }
                Err(pending_future) => future = pending_future,
            }
        }
    }

    pub(super) fn count(&self) -> usize {
        self.observed.count_ones() as usize
    }

    fn record_pending(&mut self) -> bool {
        let mut recorded = false;
        for (bit, seen) in [
            (1, &self.registration.interrupt),
            (2, &self.registration.terminate),
        ] {
            if self.observed & bit == 0 && seen.load(Ordering::SeqCst) {
                self.observed |= bit;
                recorded = true;
                observe(if self.observed.count_ones() == 1 {
                    "abort-enter-1"
                } else {
                    "abort-enter-2"
                });
            }
        }
        recorded
    }

    pub(super) async fn next(&mut self) {
        loop {
            if self.record_pending() {
                return;
            }
            // The handler only writes atomics. Polling bounds notification
            // latency without unsafe handlers or a second socket owner.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    pub(super) fn remove(&mut self) {
        if !self.removed {
            self.registration.interrupt.store(true, Ordering::SeqCst);
            self.registration.terminate.store(true, Ordering::SeqCst);
            self.registration.active.store(false, Ordering::SeqCst);
            self.removed = true;
            observe("listeners-removed");
        }
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        // Also restore default behavior on early Rust errors. Fatal-abort
        // exits intentionally omit the normal finally observation below.
        if !self.removed {
            self.registration.interrupt.store(true, Ordering::SeqCst);
            self.registration.terminate.store(true, Ordering::SeqCst);
            self.registration.active.store(false, Ordering::SeqCst);
        }
    }
}

/// Test-only observation of product state, with no scheduling/control hook.
#[cfg(not(test))]
pub(crate) fn observe(_kind: &str) {}

#[cfg(test)]
pub(crate) fn observe(kind: &str) {
    use std::{fs::OpenOptions, io::Write, sync::atomic::AtomicU64};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let Some(directory) = std::env::var_os("PDRS_SIGNAL_OBSERVER") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!("{sequence:08}-{kind}.json");
    let temporary = directory.join(format!(".{name}.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("#1145 exclusive signal observation");
    let event = serde_json::json!({"kind": kind, "seq": sequence});
    file.write_all(event.to_string().as_bytes())
        .expect("#1145 complete signal observation");
    drop(file);
    std::fs::rename(&temporary, directory.join(name))
        .expect("#1145 atomic signal observation publication");
}
