use super::super::requests::Requests;
use super::input_property;
use crate::{
    metadata::object,
    runtime::{
        connection::{Call, RequestError},
        turn::{read, replay::ActiveTurnSnapshotInput},
    },
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
            // TS reads `input.sessionKey` straight into the replay Map lookup
            // (getActiveTurnSnapshot → GatewayTurnReplayStore.snapshot,
            // GatewayTurnReplayStore.ts:107-109): a nullish params throws on
            // the read (probe a4), and any other non-string key is a Map miss
            // answered with the input's own value echoed — `sessionKey` is
            // dropped from the miss object exactly when the field is absent or
            // undefined, like JSON.stringify drops an undefined (probes
            // a1/a2/a5, #1604).
            let raw_key = input_property(&params, "sessionKey")?;
            let Some(session_key) = raw_key.as_ref().and_then(JsValue::as_string).cloned() else {
                let mut miss: Vec<(&'static str, JsValue)> = vec![("active", JsValue::Bool(false))];
                if let Some(value) = raw_key.filter(|value| !matches!(value, JsValue::Undefined)) {
                    miss.push(("sessionKey", value));
                }
                miss.push(("events", JsValue::Array(vec![])));
                return Ok(object(miss));
            };
            // `includeEvents` is read only on the hit path, after the Map
            // lookup (GatewayTurnReplayStore.ts:117), and only an explicit
            // `false` suppresses events.
            let project =
                pilotdeck_protocol::JsString::from(self.options.project.to_string_lossy().as_ref());
            Ok(read::active_turn_snapshot_for_project(
                &self.coordinator,
                &ActiveTurnSnapshotInput {
                    session_key,
                    include_events: params.get("includeEvents") != Some(&JsValue::Bool(false)),
                },
                &project,
            )
            .to_json())
        })
    }
}
