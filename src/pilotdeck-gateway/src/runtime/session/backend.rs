//! Product execution seam. Transport hosts are one backend; native Scheduler sessions are another.
use super::{LiveSession, factory::ReloadSource};
use crate::{
    runtime::{
        project::RuntimeError,
        turn::{
            host_factory::TurnHostFactory, live::LiveSessionAdapter, runner::tail::SessionTail,
        },
    },
    storage::recorder::Recorder,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::{Arc, atomic::AtomicBool};

pub type HistoryProjection = Arc<dyn Fn() -> Vec<JsValue> + Send + Sync>;
pub struct BackendContext {
    pub recorder: Arc<Recorder>,
    pub history: HistoryProjection,
    pub tail: Arc<SessionTail>,
    pub reload_source: ReloadSource,
}
/// The factory consumes session-bound resources once, after persistence restore.
/// A native backend must never manufacture a HostClient or an MP2 transport.
pub trait SessionBackendFactory: Send {
    fn build(
        self: Box<Self>,
        context: BackendContext,
    ) -> crate::runtime::refresh::BoxFuture<Result<Box<dyn SessionBackend>, RuntimeError>>;
}
pub trait SessionBackend: LiveSession {
    fn permission_mode(&self) -> JsString;
    fn execution_started(&self) -> Arc<AtomicBool>;
}
impl SessionBackendFactory for TurnHostFactory {
    fn build(
        self: Box<Self>,
        context: BackendContext,
    ) -> crate::runtime::refresh::BoxFuture<Result<Box<dyn SessionBackend>, RuntimeError>> {
        Box::pin(async move {
            let history = context.history;
            Ok(Box::new(
                (*self)
                    .into_live_session(context.recorder, move || history())
                    .with_tail(context.tail)
                    .with_reload_source(context.reload_source),
            ) as Box<dyn SessionBackend>)
        })
    }
}
impl SessionBackend for LiveSessionAdapter {
    fn permission_mode(&self) -> JsString {
        self.permission_mode()
    }
    fn execution_started(&self) -> Arc<AtomicBool> {
        self.reload_execution_started()
    }
}
