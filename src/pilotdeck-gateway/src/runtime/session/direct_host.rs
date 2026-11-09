//! In-process module host dispatch for embedded agent turns.
use crate::host::{HostError, HostHandler};
use pilotdeck_agent::ports::{CallControl, PortError, PortResult};
use pilotdeck_protocol::{JsObject, JsValue};
use std::sync::Arc;

pub struct DirectModuleHost {
    handler: Arc<dyn HostHandler>,
    runtime: tokio::runtime::Handle,
}

impl DirectModuleHost {
    pub fn new(handler: Arc<dyn HostHandler>, runtime: tokio::runtime::Handle) -> Self {
        Self { handler, runtime }
    }
}

impl pilotdeck_transport::direct_host::DirectHostCallback for DirectModuleHost {
    fn dispatch_module_call(
        &self,
        control: &CallControl,
        request: &JsValue,
    ) -> PortResult<JsObject> {
        control.check()?;
        let call = request.clone();
        let handler = self.handler.clone();
        self.runtime.block_on(async move {
            tokio::select! {
                result = handler.module_call(call) => result.map_err(host_error_to_port),
                result = async {
                    loop {
                        control.check()?;
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                } => result,
            }
        })
    }
}

fn host_error_to_port(error: HostError) -> PortError {
    PortError::HostFailure {
        code: error
            .module_error
            .as_ref()
            .and_then(|e| e.code.clone())
            .unwrap_or_else(|| "HOST_MODULE_FAILED".into()),
        message: error
            .module_error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| error.message.clone().into()),
        boundary: pilotdeck_protocol::model::BoundarySnapshot::capture(&JsValue::Null),
    }
}
