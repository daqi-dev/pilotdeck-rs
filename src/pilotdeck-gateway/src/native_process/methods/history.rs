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

fn unavailable(method: &str, label: &str) -> RequestError {
    RequestError {
        code: "CAPABILITY_UNAVAILABLE".into(),
        message: format!("{label} is not configured. Wire `{method}` via createLocalGateway.")
            .into(),
        details: None,
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
                "export_session_transcript" => Err(unavailable(
                    "exportSessionTranscript",
                    "export_session_transcript",
                )),
                "restore_session_transcript" => Err(unavailable(
                    "restoreSessionTranscript",
                    "restore_session_transcript",
                )),
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
                "read_subagent_messages" => Err(unavailable(
                    "readSubagentMessages",
                    "read_subagent_messages",
                )),
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
