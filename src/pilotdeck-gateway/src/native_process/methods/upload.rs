//! A8.2 upload lifecycle wiring. Each call runs in a blocking task so disk
//! I/O cannot stall the serial host's async event loop.
use super::super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &[
    "upload_create",
    "upload_get",
    "upload_part",
    "upload_complete",
    "upload_cancel",
];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::upload_request);

fn upload_error(e: crate::upload::UploadError) -> RequestError {
    RequestError {
        code: e.code.into(),
        message: e.message,
        details: e.details,
        validation: None,
    }
}

impl Requests {
    pub(in crate::native_process) fn upload_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        let gateway = self.upload_gateway.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || match method.as_str() {
                "upload_create" => gateway.create(&params),
                "upload_get" => gateway.get(&params),
                "upload_part" => gateway.part(&params),
                "upload_complete" => gateway.complete(&params),
                "upload_cancel" => gateway.cancel(&params),
                _ => unreachable!(),
            })
            .await
            .map_err(|e| RequestError::new(e.to_string()))?
            .map_err(upload_error)
        })
    }
}
