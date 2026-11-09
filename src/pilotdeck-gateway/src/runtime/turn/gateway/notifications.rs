//! Host-wide notifications. Connections are weakly held and the registry is
//! never locked while invoking a connection or its transport.
use super::{GatewayConnection, Job};
use crate::runtime::refresh::NotificationServer;
use pilotdeck_protocol::JsValue;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, ThreadId},
};
use tokio::sync::mpsc;

struct Entry {
    connection: Weak<GatewayConnection>,
    closed: AtomicBool,
}

pub(super) struct HostNotifications {
    jobs: mpsc::UnboundedSender<Job>,
    own: Weak<Self>,
    host_thread: OnceLock<ThreadId>,
    next_id: AtomicU64,
    connections: Mutex<BTreeMap<u64, Arc<Entry>>>,
}

impl HostNotifications {
    pub(super) fn new(jobs: mpsc::UnboundedSender<Job>) -> Arc<Self> {
        Arc::new_cyclic(|own| Self {
            jobs,
            own: own.clone(),
            host_thread: OnceLock::new(),
            next_id: AtomicU64::new(1),
            connections: Mutex::new(BTreeMap::new()),
        })
    }

    /// Called once on the actual host thread before it starts processing jobs.
    pub(super) fn bind_host_thread(&self) {
        let id = thread::current().id();
        assert!(
            *self.host_thread.get_or_init(|| id) == id,
            "notification host thread changed"
        );
    }

    pub(super) fn register(
        self: &Arc<Self>,
        connection: &Arc<GatewayConnection>,
    ) -> Arc<Registration> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = Arc::new(Entry {
            connection: Arc::downgrade(connection),
            closed: AtomicBool::new(false),
        });
        self.connections.lock().unwrap().insert(id, entry.clone());
        Arc::new(Registration {
            owner: Arc::downgrade(self),
            id,
            entry,
        })
    }

    fn snapshot(&self) -> Vec<Arc<Entry>> {
        let mut entries = self.connections.lock().unwrap();
        entries.retain(|_, entry| {
            !entry.closed.load(Ordering::Acquire) && entry.connection.strong_count() != 0
        });
        entries.values().cloned().collect()
    }

    fn deliver(entries: Vec<Arc<Entry>>, name: &str, payload: &JsValue) {
        for entry in entries {
            if !entry.closed.load(Ordering::Acquire)
                && let Some(connection) = entry.connection.upgrade()
            {
                connection.notify(name, Some(payload.clone()));
            }
        }
    }
}

impl NotificationServer for HostNotifications {
    fn broadcast_notification(&self, name: &str, payload: &JsValue) {
        if self
            .host_thread
            .get()
            .is_some_and(|id| *id == thread::current().id())
        {
            Self::deliver(self.snapshot(), name, payload);
        } else {
            // Snapshot on the host when the job runs, so closed registrations
            // cannot receive an event queued by an external thread.
            // The queued job holds no strong reference to the host registry.
            let name = name.to_owned();
            let payload = payload.clone();
            let own = self.own.clone();
            let _ = self.jobs.send(Box::pin(async move {
                if let Some(owner) = own.upgrade() {
                    Self::deliver(owner.snapshot(), &name, &payload);
                }
            }));
        }
    }
}

pub(super) struct Registration {
    owner: Weak<HostNotifications>,
    id: u64,
    entry: Arc<Entry>,
}
impl Registration {
    pub(super) fn close(&self) {
        self.entry.closed.store(true, Ordering::Release);
        if let Some(owner) = self.owner.upgrade() {
            owner.connections.lock().unwrap().remove(&self.id);
        }
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_broadcast_is_inline_and_external_broadcast_is_queued() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let owner = HostNotifications::new(tx);
        owner.bind_host_thread();
        owner.broadcast_notification("test", &JsValue::Null);
        assert!(rx.try_recv().is_err());
        let other = owner.clone();
        std::thread::spawn(move || other.broadcast_notification("test", &JsValue::Null))
            .join()
            .unwrap();
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }
    struct TestGateway;
    impl crate::runtime::connection::ConnectionGateway for TestGateway {
        fn describe_server(
            &self,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<JsValue, crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(JsValue::Null) })
        }
        fn request(
            &self,
            _: pilotdeck_protocol::JsString,
            _: JsValue,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<JsValue, crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(JsValue::Null) })
        }
        fn submit<'a>(
            &'a self,
            _: JsValue,
            _: &'a (dyn Fn(JsValue) + Send + Sync),
        ) -> crate::runtime::connection::Call<
            'a,
            Result<(), crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(()) })
        }
        fn disconnect(
            &self,
            _: JsValue,
            _: crate::runtime::connection::binding::WsBinding,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<bool, crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(false) })
        }
        fn abort_on_close(
            &self,
            _: JsValue,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<(), crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Default)]
    struct Sink {
        frames: Mutex<Vec<String>>,
        close_on_notification: Mutex<Option<Arc<Registration>>>,
    }
    impl crate::runtime::connection::GatewayTextConnection for Sink {
        fn send_text(&self, message: String) {
            if message.contains(r#""type":"notification""#)
                && let Some(registration) = self.close_on_notification.lock().unwrap().take()
            {
                registration.close();
            }
            self.frames.lock().unwrap().push(message);
        }
        fn close(&self, _: u16, _: &str) {}
    }

    #[tokio::test]
    async fn reentrant_close_skips_later_connection_from_snapshot() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let owner = HostNotifications::new(tx);
        owner.bind_host_thread();
        let first_sink = Arc::new(Sink::default());
        let second_sink = Arc::new(Sink::default());
        let make = |sink: Arc<Sink>| {
            Arc::new(GatewayConnection::new(
                sink,
                Arc::new(TestGateway),
                "token".into(),
                "test".into(),
            ))
        };
        let first = make(first_sink.clone());
        let second = make(second_sink.clone());
        let hello = r#"{"type":"hello","protocolVersion":"1.2","clientName":"ui","clientVersion":"test","token":"token"}"#;
        first.receive(hello).await.unwrap();
        second.receive(hello).await.unwrap();
        let _first_registration = owner.register(&first);
        let second_registration = owner.register(&second);
        *first_sink.close_on_notification.lock().unwrap() = Some(second_registration.clone());
        owner.broadcast_notification("test", &JsValue::Null);
        assert_eq!(
            first_sink
                .frames
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.contains(r#""type":"notification""#))
                .count(),
            1
        );
        assert_eq!(
            second_sink
                .frames
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.contains(r#""type":"notification""#))
                .count(),
            0
        );
        assert!(second_registration.entry.closed.load(Ordering::Acquire));
    }
}
