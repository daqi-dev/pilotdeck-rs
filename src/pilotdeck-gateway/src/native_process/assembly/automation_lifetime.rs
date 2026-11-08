//! Lifecycle operations stay owned by the gateway runtime when an RPC caller
//! disappears. One serial owner prevents reload/start/stop interleaving.
use std::sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, Ordering}};
use tokio::sync::{oneshot, watch};
use super::automation::{ProjectAutomationBundle, ProjectAutomationReloadInput};
use crate::runtime::network::Cancellation;

type Outcome = Result<(), String>;
type Finished = watch::Receiver<Option<Outcome>>;
pub(super) struct NativeAutomationLifetime {
    bundle: Arc<ProjectAutomationBundle>,
    runtime: OnceLock<tokio::runtime::Handle>,
    serial: Arc<tokio::sync::Mutex<()>>,
    stopping: AtomicBool,
    stopped: Mutex<Option<Finished>>,
    pub shutdown: Cancellation,
}
impl NativeAutomationLifetime {
    pub(super) fn new(bundle: Arc<ProjectAutomationBundle>) -> Arc<Self> {
        Arc::new(Self { bundle, runtime: OnceLock::new(), serial: Arc::default(),
            stopping: AtomicBool::new(false), stopped: Mutex::new(None), shutdown: Cancellation::new() })
    }
    pub(super) fn bind_runtime(&self, runtime: tokio::runtime::Handle) -> std::io::Result<()> {
        self.runtime.set(runtime).map_err(|_| std::io::Error::other("Automation runtime already bound."))
    }
    pub(super) fn is_stopping(&self) -> bool { self.stopping.load(Ordering::Acquire) }
    pub(super) async fn start(self: &Arc<Self>) -> Outcome {
        let runtime = self.runtime.get().ok_or("Automation runtime is unavailable.")?;
        let owner = self.clone(); let (send, receive) = oneshot::channel();
        runtime.spawn(async move {
            let _serial = owner.serial.lock().await;
            let result = if owner.is_stopping() { Err("Automation startup was cancelled.".into()) }
                else { owner.bundle.start().await.map(|_| ()).map_err(|error| error.to_string()) };
            let _ = send.send(result);
        });
        receive.await.map_err(|_| "Automation startup worker stopped.".to_owned())?
    }
    pub(super) async fn reload(self: &Arc<Self>, input: ProjectAutomationReloadInput) -> Outcome {
        let runtime = self.runtime.get().ok_or("Automation runtime is unavailable.")?;
        let owner = self.clone(); let (send, receive) = oneshot::channel();
        runtime.spawn(async move {
            let _serial = owner.serial.lock().await;
            let result = if owner.is_stopping() { Err("Automation reload was cancelled.".into()) }
                else { owner.bundle.reload(input).await.map(|_| ()).map_err(|error| error.to_string()) };
            let _ = send.send(result);
        });
        receive.await.map_err(|_| "Automation reload worker stopped.".to_owned())?
    }
    pub(super) fn stop(self: &Arc<Self>) -> super::automation::AutomationFuture {
        self.stopping.store(true, Ordering::Release);
        self.shutdown.cancel();
        let mut stopped = self.stopped.lock().unwrap_or_else(|error| error.into_inner());
        let mut receive = if let Some(receive) = &*stopped { receive.clone() } else {
            let (send, receive) = watch::channel(None);
            *stopped = Some(receive.clone());
            if let Some(runtime) = self.runtime.get() {
                let owner = self.clone();
                runtime.spawn(async move {
                    let _serial = owner.serial.lock().await;
                    let result = owner.bundle.stop().await.map_err(|error| error.to_string());
                    send.send_replace(Some(result));
                });
            } else { send.send_replace(Some(Ok(()))); }
            receive
        };
        drop(stopped);
        Box::pin(async move {
            loop {
                if let Some(result) = receive.borrow_and_update().clone() { return result; }
                receive.changed().await.map_err(|_| "Automation stop ended without completion.".to_owned())?;
            }
        })
    }
}
