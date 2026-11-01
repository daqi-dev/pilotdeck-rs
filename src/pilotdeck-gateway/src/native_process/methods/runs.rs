use super::super::requests::Requests;
use super::string;
use crate::{
    runtime::{
        connection::{Call, RequestError},
        turn::read::{self, RunEventsInput},
    },
    storage::RunRef,
};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &["run_get", "run_events", "run_reattach"];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::run_request);

fn read_error(error: read::TurnReadError) -> RequestError {
    RequestError {
        code: error
            .code
            .unwrap_or_else(|| "gateway_request_failed".into()),
        message: error.message,
        details: None,
        validation: None,
    }
}
impl Requests {
    pub(in crate::native_process) fn run_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            let run = RunRef {
                project_key: params
                    .get("projectKey")
                    .and_then(JsValue::as_string)
                    .cloned(),
                session_key: string(&params, "sessionKey")?,
                run_id: string(&params, "runId")?,
            };
            if method == "run_events" {
                read::run_events(
                    Some(self.runs.as_ref()),
                    &RunEventsInput {
                        run,
                        after_seq: params.get("afterSeq").cloned(),
                        limit: params.get("limit").cloned(),
                    },
                )
                .await
                .map_err(read_error)
            } else {
                read::run_get(Some(self.runs.as_ref()), &run)
                    .await
                    .map(|v| v.unwrap_or(JsValue::Null))
                    .map_err(read_error)
            }
        })
    }
}
