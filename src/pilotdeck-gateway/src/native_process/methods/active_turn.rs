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
            // the read (probe a4). #2692 fix-r1 (sibling#2): that Map is the
            // router domain — a null request meets a null-born turn while
            // "" / "null" miss it (ts-prodkey-probe cell b) — and both the
            // hit and the miss echo the input's own value, dropped exactly
            // when the field is absent or undefined, like JSON.stringify
            // drops an undefined (probes a1/a2/a5, #1604). A spelling that
            // throws in the registry template (a too-deep array) is still a
            // reference key for the Map in TS; Rust has no spelling that can
            // consult it, so it answers the miss shape (registered
            // divergence).
            let raw_key = input_property(&params, "sessionKey")?;
            let miss = |raw: Option<JsValue>| {
                let mut miss: Vec<(&'static str, JsValue)> = vec![("active", JsValue::Bool(false))];
                if let Some(value) = raw.filter(|value| !matches!(value, JsValue::Undefined)) {
                    miss.push(("sessionKey", value));
                }
                miss.push(("events", JsValue::Array(vec![])));
                object(miss)
            };
            let lookup = match raw_key.as_ref() {
                None | Some(JsValue::Undefined) => crate::js_value::router_key(&JsValue::Undefined),
                Some(value) => crate::js_value::router_key(value),
            };
            let Some(session_key) = lookup.ok() else {
                return Ok(miss(raw_key));
            };
            // `includeEvents` is read only on the hit path, after the Map
            // lookup (GatewayTurnReplayStore.ts:117), and only an explicit
            // `false` suppresses events.
            let snapshot = read::active_turn_snapshot(
                &self.coordinator,
                &ActiveTurnSnapshotInput {
                    session_key,
                    include_events: params.get("includeEvents") != Some(&JsValue::Bool(false)),
                },
            )
            .to_json();
            Ok(read::echo_request_session_key(snapshot, raw_key.as_ref()))
        })
    }
}
