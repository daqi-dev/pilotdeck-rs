use super::super::requests::Requests;
use super::string;
use crate::runtime::{
    connection::{Call, RequestError},
    turn::{read, replay::ActiveTurnSnapshotInput},
};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &["active_turn_snapshot"];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(|requests, _, params| requests.active_turn_request(params));

impl Requests {
    pub(in crate::native_process) fn active_turn_request(
        &self,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            Ok(read::active_turn_snapshot(
                &self.coordinator,
                &ActiveTurnSnapshotInput {
                    session_key: string(&params, "sessionKey")?,
                    include_events: params.get("includeEvents") != Some(&JsValue::Bool(false)),
                },
            )
            .to_json())
        })
    }
}
