use super::super::requests::Requests;
use crate::runtime::{
    connection::{Call, RequestError},
    replacement::ReplacementError,
    turn::read::{self, SessionHistoryReader},
};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &[
    "export_session_transcript",
    "restore_session_transcript",
    "read_session_messages",
    "fork_session",
    "replace_last_turn",
    "finalize_last_turn_replacement",
    "read_subagent_messages",
];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::history_request);

fn replacement_error(error: ReplacementError) -> RequestError {
    RequestError {
        code: error.code.unwrap_or("gateway_request_failed").into(),
        message: error.message.into(),
        details: error.details.map(|d| Box::new(*d)),
        validation: None,
    }
}

impl Requests {
    pub(in crate::native_process) fn history_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        let replacement = self.replacement.clone();
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            match method.as_str() {
                "export_session_transcript" => self.export_session_transcript(&params).await,
                "restore_session_transcript" => self.restore_session_transcript(&params).await,
                "read_session_messages" => read::read_session_messages(
                    &coordinator,
                    None::<&dyn SessionHistoryReader>,
                    &params,
                )
                .await
                .map_err(|error| RequestError {
                    code: error
                        .code
                        .unwrap_or_else(|| "gateway_request_failed".into()),
                    message: error.message,
                    details: None,
                    validation: None,
                }),
                "read_subagent_messages" => Err(RequestError {
                    code: "gateway_request_failed".into(),
                    message: "read_subagent_messages is not configured. Wire `readSubagentMessages` via createLocalGateway.".into(),
                    details: None,
                    validation: None,
                }),
                "fork_session" => replacement
                    .fork_session(params)
                    .await
                    .map_err(replacement_error),
                "replace_last_turn" => replacement
                    .replace_last_turn(params)
                    .await
                    .map_err(replacement_error),
                "finalize_last_turn_replacement" => replacement
                    .finalize_last_turn_replacement(params)
                    .await
                    .map_err(replacement_error),
                _ => Err(RequestError::new("Unknown history method.")),
            }
        })
    }
}
