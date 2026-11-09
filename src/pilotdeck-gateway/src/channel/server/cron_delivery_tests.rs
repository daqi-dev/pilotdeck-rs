use super::*;
use crate::channel::im_cron::CronResultDelivery;
struct Handle;
impl ChannelHandle for Handle {
    fn stop(&self, _: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}
struct Adapter {
    key: ChannelKey,
    receipts: Arc<Mutex<Vec<CronResultDelivery>>>,
    server: ChannelServer,
}
impl ChannelAdapter for Adapter {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        _: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        Box::pin(async { Ok(Arc::new(Handle) as Arc<dyn ChannelHandle>) })
    }
    fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
        let receipts = self.receipts.clone();
        let server = self.server.clone();
        let delivery = delivery.clone();
        Box::pin(async move {
            assert!(
                server.state_available(),
                "delivery future must not hold lifecycle state lock"
            );
            receipts.lock().unwrap().push(delivery);
            true
        })
    }
}
#[tokio::test]
async fn result_dispatch_uses_running_origin_then_fallback_and_preserves_empty_origin() {
    let server = ChannelServer::new(Default::default(), Arc::new(|| Box::pin(async { Ok(()) })));
    let receipts = Arc::new(Mutex::new(Vec::new()));
    server
        .hot_start_channel(Arc::new(Adapter {
            key: ChannelKey::new("feishu"),
            receipts: receipts.clone(),
            server: server.clone(),
        }))
        .await
        .unwrap();
    let mut delivery = CronResultDelivery {
        task_id: "task".into(),
        run_id: "run".into(),
        session_key: "cron:task".into(),
        channel_key: ChannelKey::new("cron"),
        origin_session_key: Some("feishu:chat=fixture:general".into()),
        origin_channel_key: Some(ChannelKey::new("feishu")),
        text: "actual result 😀".into(),
    };
    assert!(server.deliver_cron_result(delivery.clone()).await);
    delivery.origin_channel_key = None;
    delivery.channel_key = ChannelKey::new("feishu");
    assert!(server.deliver_cron_result(delivery.clone()).await);
    delivery.origin_channel_key = Some(ChannelKey::new(""));
    assert!(!server.deliver_cron_result(delivery.clone()).await);
    assert_eq!(receipts.lock().unwrap().len(), 2);
    assert_eq!(receipts.lock().unwrap()[0].text, delivery.text);
    server.close().await.unwrap();
    delivery.origin_channel_key = None;
    assert!(!server.deliver_cron_result(delivery).await);
}
