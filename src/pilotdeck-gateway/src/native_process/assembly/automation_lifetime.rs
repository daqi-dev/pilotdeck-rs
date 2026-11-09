//! Lifecycle operations stay owned by the gateway runtime when an RPC caller
//! disappears. One serial owner prevents reload/start/stop interleaving.
use super::automation::{ProjectAutomationBundle, ProjectAutomationReloadInput};
use crate::runtime::network::Cancellation;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{oneshot, watch};

type Outcome = Result<(), String>;
type Finished = watch::Receiver<Option<Outcome>>;
pub(super) struct NativeAutomationLifetime {
    bundle: Arc<ProjectAutomationBundle>,
    runtime: OnceLock<tokio::runtime::Handle>,
    serial: Arc<tokio::sync::Mutex<()>>,
    stopping: AtomicBool,
    stopped: Mutex<Option<Finished>>,
    reload_tail: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub shutdown: Cancellation,
}
impl NativeAutomationLifetime {
    pub(super) fn new(bundle: Arc<ProjectAutomationBundle>) -> Arc<Self> {
        Arc::new(Self {
            bundle,
            runtime: OnceLock::new(),
            serial: Arc::default(),
            stopping: AtomicBool::new(false),
            stopped: Mutex::new(None),
            reload_tail: Mutex::new(None),
            shutdown: Cancellation::new(),
        })
    }
    pub(super) fn bind_runtime(&self, runtime: tokio::runtime::Handle) -> std::io::Result<()> {
        self.runtime
            .set(runtime)
            .map_err(|_| std::io::Error::other("Automation runtime already bound."))
    }
    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }
    pub(super) async fn start(self: &Arc<Self>) -> Outcome {
        let runtime = self
            .runtime
            .get()
            .ok_or("Automation runtime is unavailable.")?;
        let owner = self.clone();
        let (send, receive) = oneshot::channel();
        runtime.spawn(async move {
            let _serial = owner.serial.lock().await;
            let result = if owner.is_stopping() {
                Err("Automation startup was cancelled.".into())
            } else {
                owner
                    .bundle
                    .start()
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            };
            let _ = send.send(result);
        });
        receive
            .await
            .map_err(|_| "Automation startup worker stopped.".to_owned())?
    }
    pub(super) async fn reload(self: &Arc<Self>, input: ProjectAutomationReloadInput) -> Outcome {
        let runtime = self
            .runtime
            .get()
            .ok_or("Automation runtime is unavailable.")?;
        let owner = self.clone();
        let (send, receive) = oneshot::channel();
        runtime.spawn(async move {
            let _serial = owner.serial.lock().await;
            let result = if owner.is_stopping() {
                Err("Automation reload was cancelled.".into())
            } else {
                owner
                    .bundle
                    .reload(input)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            };
            let _ = send.send(result);
        });
        receive
            .await
            .map_err(|_| "Automation reload worker stopped.".to_owned())?
    }
    /// Called synchronously by the config-store listener; preserve publication
    /// order even when several reload RPC callers cancel their awaits.
    #[cfg(test)]
    pub(super) fn request_reload(self: &Arc<Self>, input: ProjectAutomationReloadInput) {
        self.request_config_reload(None, Some(input));
    }

    /// One config publication enters one ordered reload operation. Adapters
    /// run first, then automation, as in the fixed CLI's reloadChain. Closing
    /// the host awaits this same tail; queued work cannot resurrect a channel.
    pub(super) fn request_config_reload(
        self: &Arc<Self>,
        channels: Option<(
            Arc<crate::native_process::channels::NativeChannels>,
            pilotdeck_protocol::JsValue,
        )>,
        input: Option<ProjectAutomationReloadInput>,
    ) {
        if self.is_stopping() {
            return;
        }
        let Some(runtime) = self.runtime.get() else {
            return;
        };
        let mut tail = self
            .reload_tail
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.is_stopping() {
            return;
        }
        let previous = tail.take();
        let owner = self.clone();
        *tail = Some(runtime.spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            if let Some((channels, config)) = channels {
                let _serial = owner.serial.lock().await;
                if !owner.is_stopping() {
                    match channels.reload(&config).await {
                        Ok(result) => {
                            let parts = result
                                .started
                                .iter()
                                .map(|key| format!("{key}=started"))
                                .chain(result.stopped.iter().map(|key| format!("{key}=stopped")))
                                .collect::<Vec<_>>();
                            if !parts.is_empty() {
                                crate::native_process::channels::console_log_line(
                                    &mut std::io::stdout(),
                                    &format!(
                                        "[pilotdeck] Adapter hot-reload complete: {}",
                                        parts.join(", ")
                                    ),
                                );
                            }
                        }
                        Err(error) => {
                            eprintln!("[pilotdeck] adapter hot-reload failed: {}", error.message())
                        }
                    }
                }
            }
            if let Some(input) = input {
                if let Err(error) = owner.reload(input).await {
                    eprintln!("[automation] reload failed: {error}");
                }
            }
        }));
    }

    pub(super) fn stop(self: &Arc<Self>) -> super::automation::AutomationFuture {
        self.stopping.store(true, Ordering::Release);
        self.shutdown.cancel();
        let mut stopped = self
            .stopped
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut receive = if let Some(receive) = &*stopped {
            receive.clone()
        } else {
            let (send, receive) = watch::channel(None);
            *stopped = Some(receive.clone());
            if let Some(runtime) = self.runtime.get() {
                let owner = self.clone();
                let reload_tail = self
                    .reload_tail
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                runtime.spawn(async move {
                    if let Some(reload_tail) = reload_tail {
                        let _ = reload_tail.await;
                    }
                    let _serial = owner.serial.lock().await;
                    let result = owner.bundle.stop().await.map_err(|error| error.to_string());
                    send.send_replace(Some(result));
                });
            } else {
                send.send_replace(Some(Ok(())));
            }
            receive
        };
        drop(stopped);
        Box::pin(async move {
            loop {
                if let Some(result) = receive.borrow_and_update().clone() {
                    return result;
                }
                receive
                    .changed()
                    .await
                    .map_err(|_| "Automation stop ended without completion.".to_owned())?;
            }
        })
    }
}
