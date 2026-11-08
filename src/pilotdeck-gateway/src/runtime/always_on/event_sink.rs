//! Tracked Always-On phase event forwarding and bounded persistence.
use std::{future::Future, pin::Pin, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}, time::{SystemTime, UNIX_EPOCH}};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use super::{apply::ApplyEvents, manager::{ControlError, PhaseObserver}, reports::DiscoveryReportStore};

pub(crate) type TurnEventBroadcast = Arc<dyn Fn(&JsString, &JsString, &JsValue) + Send + Sync>;
pub(crate) type EventDiagnostic = Arc<dyn Fn(&'static str) + Send + Sync>;
#[derive(Default)]
struct Pending { count: AtomicUsize, changed: tokio::sync::Notify }
struct Ticket(Arc<Pending>);
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 { self.0.changed.notify_waiters(); }
    }
}

#[derive(Clone)]
pub(crate) struct PhaseEventSink {
    pub project: JsString,
    pub store: DiscoveryReportStore,
    pub broadcast: Option<TurnEventBroadcast>,
    diagnostic: EventDiagnostic,
    failures: Arc<AtomicUsize>,
    pending: Arc<Pending>,
    last_event: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}
impl PhaseEventSink {
    pub(crate) fn new(project: JsString, store: DiscoveryReportStore,
        broadcast: Option<TurnEventBroadcast>, diagnostic: EventDiagnostic) -> Self {
        Self { project, store, broadcast, diagnostic, failures: Arc::new(AtomicUsize::new(0)),
            pending: Arc::default(), last_event: Arc::new(Mutex::new(None)) }
    }
    pub(crate) fn failed_writes(&self) -> usize { self.failures.load(Ordering::Acquire) }
    /// Call after the producer/phase has stopped admitting new event work.
    pub(crate) async fn drain(&self) {
        let last = self.last_event.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(last) = last && last.await.is_err() { Self::diagnose(&self.failures, &self.diagnostic); }
        loop {
            let wake = self.pending.changed.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            if self.pending.count.load(Ordering::Acquire) == 0 { return; }
            wake.await;
        }
    }
    fn ticket(&self) -> Ticket {
        self.pending.count.fetch_add(1, Ordering::AcqRel);
        Ticket(self.pending.clone())
    }
    fn diagnose(failures: &AtomicUsize, diagnostic: &EventDiagnostic) {
        failures.fetch_add(1, Ordering::AcqRel);
        diagnostic("Always-On event persistence failed");
    }
    pub(crate) fn emit_phase(&self, run_id: &str, phase: &str, plan: Option<&JsString>,
        title: Option<&JsString>, outcome: Option<&str>, error: Option<&ControlError>) {
        let mut event = JsObject::new();
        event.insert("schemaVersion".into(), JsValue::Number(1.0));
        event.insert("eventId".into(), uuid::Uuid::new_v4().to_string().as_str().into());
        event.insert("runId".into(), run_id.into());
        event.insert("projectKey".into(), JsValue::String(self.project.clone()));
        event.insert("phase".into(), phase.into());
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0);
        event.insert("timestamp".into(), crate::runtime::cron::iso_from_epoch_ms(now).as_str().into());
        if let Some(plan) = plan { event.insert("planId".into(), JsValue::String(plan.clone())); }
        if let Some(title) = title { event.insert("title".into(), JsValue::String(title.clone())); }
        if let Some(outcome) = outcome { event.insert("outcome".into(), outcome.into()); }
        if let Some(error) = error {
            let mut data = JsObject::new(); data.insert("code".into(), error.code.as_str().into());
            data.insert("message".into(), error.message.as_str().into()); event.insert("error".into(), JsValue::Object(data));
        }
        let ticket = self.ticket(); let store = self.store.clone(); let failures = self.failures.clone();
        let diagnostic = self.diagnostic.clone();
        let mut last = self.last_event.lock().unwrap_or_else(|e| e.into_inner());
        let previous = last.take();
        *last = Some(tokio::spawn(async move {
            let _ticket = ticket;
            if let Some(previous) = previous && previous.await.is_err() { Self::diagnose(&failures, &diagnostic); }
            if store.append_event(event).await.is_err() { Self::diagnose(&failures, &diagnostic); }
        }));
    }
}
impl ApplyEvents for PhaseEventSink {
    fn drain<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(PhaseEventSink::drain(self))
    }
    fn emit(&self, run_id: &str, phase: &str, error: Option<&ControlError>) {
        self.emit_phase(run_id, phase, None, None, Some(if error.is_some() { "failed" } else { "executed" }), error);
    }
}
impl PhaseObserver for PhaseEventSink {
    fn on_event<'a>(&'a self, session: &'a JsString, channel: &'a JsString,
        phase_run: &'a JsString, event: &'a JsValue) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        if let Some(broadcast) = &self.broadcast { broadcast(session, channel, event); }
        let event = event.as_object().cloned(); let run = phase_run.clone();
        let store = self.store.clone(); let failures = self.failures.clone(); let ticket = self.ticket();
        let diagnostic = self.diagnostic.clone();
        let job = tokio::spawn(async move {
            let _ticket = ticket;
            match event {
                Some(value) => if store.append_run_event(run, value).await.is_err() { Self::diagnose(&failures, &diagnostic); },
                None => Self::diagnose(&failures, &diagnostic),
            }
        });
        Box::pin(async move { if job.await.is_err() { Self::diagnose(&self.failures, &self.diagnostic); } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn final_ticket_wakes_every_concurrent_drainer() {
        let root = std::env::temp_dir().join(format!("pdrs-event-drain-{}", uuid::Uuid::new_v4()));
        let metadata = crate::metadata::Metadata { home: root.clone(), cwd: Default::default(), files: crate::metadata::NativeFiles };
        let paths = super::super::paths::resolve_always_on_paths(&root, "/fixture/project", &metadata).unwrap();
        let sink = PhaseEventSink::new("/fixture/project".into(), DiscoveryReportStore::new(paths), None, Arc::new(|_| {}));
        let ticket = sink.ticket();
        let mut tasks = Vec::new();
        for _ in 0..3 {
            let sink = sink.clone();
            let (ready, parked) = tokio::sync::oneshot::channel();
            tasks.push(tokio::spawn(async move {
                let drain = sink.drain();
                tokio::pin!(drain);
                std::future::poll_fn(|cx| {
                    assert!(drain.as_mut().poll(cx).is_pending());
                    std::task::Poll::Ready(())
                }).await;
                ready.send(()).unwrap();
                drain.await;
            }));
            parked.await.unwrap();
        }
        drop(ticket);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            for task in tasks { task.await.unwrap(); }
        }).await.expect("the final write must release every waiter, including stop");
    }
    #[tokio::test]
    async fn observer_and_synthetic_events_are_persisted_and_drained() {
        let root = std::env::temp_dir().join(format!("pdrs-always-on-events-{}", uuid::Uuid::new_v4()));
        let metadata = crate::metadata::Metadata { home: root.clone(), cwd: Default::default(), files: crate::metadata::NativeFiles };
        let paths = super::super::paths::resolve_always_on_paths(&root, "/fixture/project", &metadata).unwrap();
        let event_path = paths.events_file(); let run_path = paths.run_events_path(&"run.apply".into());
        let messages = Arc::new(Mutex::new(Vec::<String>::new())); let captured = messages.clone();
        let sink = PhaseEventSink::new("/fixture/project".into(), DiscoveryReportStore::new(paths),
            Some(Arc::new(move |_, _, _| captured.lock().unwrap().push("broadcast".into()))), Arc::new(|_| {}));
        sink.emit("run", "apply_started", None);
        let mut event = JsObject::new(); event.insert("type".into(), "text_delta".into());
        sink.on_event(&"session".into(), &"always-on/apply".into(), &"run.apply".into(), &JsValue::Object(event)).await;
        sink.emit("run", "apply_completed", None);
        sink.drain().await;
        assert_eq!(sink.failed_writes(), 0);
        assert_eq!(*messages.lock().unwrap(), ["broadcast"]);
        let events = std::fs::read_to_string(event_path).unwrap();
        let rows: Vec<_> = events.lines().collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("apply_started"));
        assert!(rows[1].contains("apply_completed"));
        assert!(std::fs::read_to_string(run_path).unwrap().contains("text_delta"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
