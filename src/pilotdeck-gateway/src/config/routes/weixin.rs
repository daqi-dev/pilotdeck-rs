mod attachment_net;
mod attachments;
mod http;
mod item_types;
mod media;
mod inbox;
pub use inbox::WeixinInbox;
mod native;
mod storage;
pub use native::{NativeWeixinFactory, WeixinDelivery};
// Native Weixin replacement lifecycle; network/login and mapper persistence are
// owned by the installed channel factory, not inferred from config.enabled.
use super::{Result, RouteError, WeixinLoginHost};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub type ChannelCall<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
pub trait WeixinChannel: Send + Sync {
    /// Return once the channel handle starts; QR login continues in its background task.
    fn start(&self) -> ChannelCall<'_, ()>;
    fn stop(&self) -> ChannelCall<'_, ()>;
}
pub trait WeixinChannelFactory: Send + Sync {
    /// Load the persisted mapper and attach its persistence callback to the new channel.
    fn load_and_create(&self) -> ChannelCall<'_, Arc<dyn WeixinChannel>>;
    fn attach_mapper_persistence(&self, _saver: Arc<dyn Fn(serde_json::Value) + Send + Sync>) {}
}
pub struct WeixinChannelHost {
    factory: Arc<dyn WeixinChannelFactory>,
    current: tokio::sync::Mutex<Option<Arc<dyn WeixinChannel>>>,
    closing: AtomicBool,
}
impl WeixinChannelHost {
    pub fn new(factory: Arc<dyn WeixinChannelFactory>) -> Self {
        Self {
            factory,
            current: tokio::sync::Mutex::new(None),
            closing: AtomicBool::new(false),
        }
    }
    pub fn attach_mapper_persistence(&self, saver: Arc<dyn Fn(serde_json::Value) + Send + Sync>) {
        self.factory.attach_mapper_persistence(saver);
    }
    pub async fn close(&self) -> Result<()> {
        self.closing.store(true, Ordering::SeqCst);
        self.stop_current().await
    }
    /// Stop the active adapter without sealing this owner during config reload.
    pub async fn stop_current(&self) -> Result<()> {
        let mut current = self.current.lock().await;
        if let Some(channel) = current.as_ref() {
            channel.stop().await?;
        }
        *current = None;
        Ok(())
    }
}
impl WeixinLoginHost for WeixinChannelHost {
    fn shutdown(&self) -> ChannelCall<'_, ()> {
        Box::pin(self.close())
    }

    fn hot_start(&self) -> ChannelCall<'_, ()> {
        Box::pin(async {
            let next = self.factory.load_and_create().await?;
            let mut current = self.current.lock().await;
            if self.closing.load(Ordering::SeqCst) {
                return Err(RouteError::new(
                    "gateway_request_failed",
                    "channel server is closing",
                ));
            }
            let previous = current.clone();
            if let Some(previous) = &previous {
                previous.stop().await?;
            }
            *current = None;
            if let Err(error) = next.start().await {
                if let Some(previous) = previous {
                    if previous.start().await.is_err() {
                        return Err(RouteError::new(
                            "gateway_request_failed",
                            "channel weixin replacement and rollback failed",
                        ));
                    }
                    *current = Some(previous);
                }
                return Err(error);
            }
            *current = Some(next);
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};
    struct Channel {
        id: usize,
        starts: Mutex<VecDeque<bool>>,
        events: Arc<Mutex<Vec<String>>>,
    }
    impl WeixinChannel for Channel {
        fn start(&self) -> ChannelCall<'_, ()> {
            Box::pin(async {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("start:{}", self.id));
                if self.starts.lock().unwrap().pop_front().unwrap_or(true) {
                    Ok(())
                } else {
                    Err(RouteError::new("gateway_request_failed", "start failed"))
                }
            })
        }
        fn stop(&self) -> ChannelCall<'_, ()> {
            Box::pin(async {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("stop:{}", self.id));
                Ok(())
            })
        }
    }
    struct Factory {
        channels: Mutex<VecDeque<Arc<dyn WeixinChannel>>>,
        events: Arc<Mutex<Vec<String>>>,
    }
    impl WeixinChannelFactory for Factory {
        fn load_and_create(&self) -> ChannelCall<'_, Arc<dyn WeixinChannel>> {
            Box::pin(async {
                self.events
                    .lock()
                    .unwrap()
                    .push("load-mapper:create-with-persistence".into());
                self.channels
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| RouteError::new("gateway_request_failed", "factory empty"))
            })
        }
    }
    #[tokio::test]
    async fn hot_start_replaces_serially_restores_previous_and_rejects_closing() {
        let events = Arc::new(Mutex::new(vec![]));
        let channel = |id, starts| {
            Arc::new(Channel {
                id,
                starts: Mutex::new(starts),
                events: events.clone(),
            }) as Arc<dyn WeixinChannel>
        };
        let factory = Arc::new(Factory {
            channels: Mutex::new(VecDeque::from([
                channel(1, VecDeque::from([true, true])),
                channel(2, VecDeque::from([false])),
                channel(3, VecDeque::from([true])),
                channel(4, VecDeque::from([true])),
            ])),
            events: events.clone(),
        });
        let host = WeixinChannelHost::new(factory);
        host.hot_start().await.unwrap();
        assert_eq!(host.hot_start().await.unwrap_err().message, "start failed");
        host.hot_start().await.unwrap();
        host.close().await.unwrap();
        assert_eq!(
            host.hot_start().await.unwrap_err().message,
            "channel server is closing"
        );
        assert_eq!(
            *events.lock().unwrap(),
            [
                "load-mapper:create-with-persistence",
                "start:1",
                "load-mapper:create-with-persistence",
                "stop:1",
                "start:2",
                "start:1",
                "load-mapper:create-with-persistence",
                "stop:1",
                "start:3",
                "stop:3",
                "load-mapper:create-with-persistence"
            ]
        );
    }
    #[tokio::test]
    async fn rollback_failure_is_an_error_and_concurrent_requests_share_one_queue() {
        let events = Arc::new(Mutex::new(vec![]));
        let first = Arc::new(Channel {
            id: 1,
            starts: Mutex::new(VecDeque::from([true, false])),
            events: events.clone(),
        }) as Arc<dyn WeixinChannel>;
        let second = Arc::new(Channel {
            id: 2,
            starts: Mutex::new(VecDeque::from([false])),
            events: events.clone(),
        }) as Arc<dyn WeixinChannel>;
        let host = WeixinChannelHost::new(Arc::new(Factory {
            channels: Mutex::new(VecDeque::from([first, second])),
            events,
        }));
        let (a, b) = tokio::join!(host.hot_start(), host.hot_start());
        a.unwrap();
        assert_eq!(
            b.unwrap_err().message,
            "channel weixin replacement and rollback failed"
        );
    }
}

#[cfg(test)]
#[path = "weixin/tests.rs"]
mod native_tests;
