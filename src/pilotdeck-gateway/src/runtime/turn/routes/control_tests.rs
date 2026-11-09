use super::*;
use crate::runtime::{
    project::{DisposeFuture, RuntimeError, RuntimeResult},
    refresh::BoxFuture,
    session::{
        LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwnerOptions,
    },
    steer::{CancelResult, EnqueueResult, SteerMailbox},
};
use crate::scratch::Scratch;
use pilotdeck_protocol::parse_json;
use std::sync::Arc;

struct Session(SteerMailbox);
impl LiveSession for Session {
    fn snapshot(&self) -> JsValue {
        JsValue::Null
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn steer(
        &self,
        turn: JsString,
        message: SteerMessage,
    ) -> crate::host::HostFuture<EnqueueResult> {
        self.0.enqueue(turn, message)
    }
    fn cancel_steer(
        &self,
        turn: JsString,
        item: JsString,
    ) -> crate::host::HostFuture<CancelResult> {
        self.0.cancel(turn, item)
    }
}
struct Factory(Arc<Session>);
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let session = self.0.clone();
        Box::pin(async move { Ok(LiveSessionHandle::new(session, None)) })
    }
}
fn json(input: &str) -> JsValue {
    parse_json(input).unwrap()
}

fn owner(mailbox: SteerMailbox) -> SessionOwner {
    let mut options = SessionOwnerOptions::new(Arc::new(Factory(Arc::new(Session(mailbox)))));
    options.idle_sweep_interval_ms = 0;
    SessionOwner::new(options)
}
async fn publish(owner: &SessionOwner) {
    owner
        .get_or_create(SessionContext {
            session_key: "s".into(),
            project_key: None,
            channel_key: "cli".into(),
            allowed_tools: None,
            disallowed_tools: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn inactive_and_mismatched_runs_short_circuit_invalid_attachments() {
    let mailbox = SteerMailbox::new(None);
    let owner = owner(mailbox);
    let composer = ProductionSteerTurnComposer::new("/tmp");
    let routes = ControlRoutes::new(&owner, &composer);
    let params = json(r#"{"sessionKey":"s","runId":"old","attachments":42}"#);
    assert_eq!(
        routes.steer_turn(&params).await.unwrap(),
        response("accepted", false, Some(SteerReason::NoActiveTurn))
    );
    owner.begin_turn(&"s".into(), &"t".into()).unwrap();
    assert_eq!(
        routes.steer_turn(&params).await.unwrap(),
        response("accepted", false, Some(SteerReason::TurnMismatch))
    );
    assert_eq!(
        routes.cancel_steer(&params).await.unwrap(),
        response("cancelled", false, Some(SteerReason::TurnMismatch))
    );
    assert!(routes.handle("abort_turn", &params).is_none());
}

#[tokio::test]
async fn steer_uses_the_live_queue_preserves_metadata_and_cancel_is_idempotent() {
    let mailbox = SteerMailbox::new(None);
    mailbox.start("t".into());
    let owner = owner(mailbox.clone());
    publish(&owner).await;
    owner.begin_turn(&"s".into(), &"t".into()).unwrap();
    let composer = ProductionSteerTurnComposer::new("/tmp");
    let routes = ControlRoutes::new(&owner, &composer);
    let params = json(r#"{"sessionKey":"s","runId":"t","itemId":"i","message":"\ud800修改"}"#);
    assert_eq!(
        routes.steer_turn(&params).await.unwrap(),
        response("accepted", true, None)
    );
    assert_eq!(
        routes.steer_turn(&params).await.unwrap(),
        response("accepted", true, None)
    );
    let drained = mailbox.drain(&"t".into());
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].message.get("metadata"),
        Some(&json(r#"{"purpose":"mid_turn_steer","queueItemId":"i"}"#))
    );
    assert_eq!(
        drained[0].message.get("content"),
        Some(&json(r#"[{"type":"text","text":"\ud800修改"}]"#))
    );
    assert_eq!(
        routes.cancel_steer(&params).await.unwrap(),
        response("cancelled", false, Some(SteerReason::TooLate))
    );
    let cancel = json(r#"{"sessionKey":"s","runId":"t","itemId":"next"}"#);
    assert_eq!(
        routes.cancel_steer(&cancel).await.unwrap(),
        response("cancelled", true, None)
    );
    assert_eq!(
        routes.cancel_steer(&cancel).await.unwrap(),
        response("cancelled", true, None)
    );
}

#[tokio::test]
async fn composer_prepares_inline_attachments_and_registered_symlink_paths() {
    let scratch = Scratch::new("steer");
    let root = scratch.path();
    let real = root.join("real.txt");
    let linked = root.join("linked.txt");
    std::fs::write(&real, "from disk").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    #[cfg(not(unix))]
    std::fs::write(&linked, "from disk").unwrap();
    let params = json(
        r#"{"message":"look","attachments":[{"type":"text","content":"inline"},{"type":"image","mimeType":"image/png","content":"base64","bytes":4},{"type":"file","path":"linked.txt","metadata":{"channelKey":"cli"}}]}"#,
    );
    let composer = ProductionSteerTurnComposer::new(root);
    let prepared = composer.prepare(&params).await.unwrap();
    assert_eq!(prepared.content.len(), 5);
    assert_eq!(prepared.content[0].get("text"), Some(&"look".into()));
    assert_eq!(prepared.content[2].get("source"), Some(&"base64".into()));
    assert!(
        prepared
            .allowed_read_files
            .contains(&JsString::from(linked.to_string_lossy().as_ref()))
    );
    assert!(
        prepared.allowed_read_files.contains(&JsString::from(
            std::fs::canonicalize(&real)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        ))
    );
    assert!(
        prepared.content[3]
            .get("text")
            .and_then(JsValue::as_string)
            .unwrap()
            .to_utf8()
            .unwrap()
            .contains("from disk")
    );
}

#[test]
fn image_metadata_rejects_truncated_headers_and_zero_dimensions() {
    assert_eq!(image_metadata_mime(b"\x89PNG\r\n\x1a\n"), None);
    assert_eq!(image_metadata_mime(b"GIF89a\0\0\0\0\0\0\0"), None);
    assert_eq!(image_metadata_mime(b"\xff\xd8\xff\xc0\0\x20"), None);
}
