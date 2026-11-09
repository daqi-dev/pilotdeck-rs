//! Text aggregation for a single turn; caller handles interaction and media events.
use pilotdeck_protocol::{JsString, JsValue};

#[derive(Default)]
pub(crate) struct TurnText {
    units: Vec<u16>,
}
impl TurnText {
    pub(crate) fn append(&mut self, event: &JsValue) -> Result<(), String> {
        if let Some(fragment) = super::render::render(event).map_err(str::to_owned)? {
            self.units.extend_from_slice(fragment.units());
        }
        Ok(())
    }
    pub(crate) fn finish(self) -> JsString {
        JsString::from_utf16(crate::runtime::memory::js_trim_units(&self.units).to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn accumulates_only_visible_fragments_and_trims_the_whole_result() {
        let mut turn = TurnText::default();
        for event in [
            r#"{"type":"assistant_thinking_delta","text":"private"}"#,
            r#"{"type":"assistant_text_delta","text":" a "}"#,
            r#"{"type":"assistant_text_delta","text":"b "}"#,
        ] {
            turn.append(&parse_json(event).unwrap()).unwrap();
        }
        assert_eq!(turn.finish(), JsString::from("a b"));
    }
}

const DELIVERABLE_HINT: &str = "[WeCom attachment hint: If the user explicitly asks you to send a generated file to this WeCom chat, save it to an absolute local path and include MEDIA:/absolute/path in your final answer. Do not use MEDIA for files that were only read or analyzed internally.]";

/// The A7 port is shared with every gateway caller; no local session owner is created.
pub(crate) async fn submit(
    gateway: &dyn crate::channel::protocol::TurnGateway,
    session_key: JsString,
    project_key: Option<JsString>,
    message: JsString,
    attachments: Vec<JsValue>,
) -> Result<
    Box<dyn crate::channel::protocol::TurnEventStream>,
    crate::channel::protocol::ChannelError,
> {
    use crate::channel::protocol::{ChannelKey, ChannelTurnOptions, TurnRequest};
    let mut hint = pilotdeck_protocol::JsObject::new();
    hint.insert("text".into(), DELIVERABLE_HINT.into());
    hint.insert("purpose".into(), "wecom_deliverable_hint".into());
    gateway
        .submit_with_options(
            TurnRequest {
                session_key,
                channel_key: ChannelKey::new("wecom"),
                message,
            },
            ChannelTurnOptions {
                project_key,
                attachments,
                allow_plan_mode_tools: Some(false),
                synthetic_messages: vec![JsValue::Object(hint)],
                ..Default::default()
            },
        )
        .await
}

#[cfg(test)]
mod gateway_tests {
    use super::*;
    use crate::channel::protocol::{
        BoxFuture, ChannelError, ChannelTurnOptions, TurnEventStream, TurnGateway, TurnRequest,
    };
    use std::sync::{Arc, Mutex};
    struct Fake(Arc<Mutex<Option<(TurnRequest, ChannelTurnOptions)>>>);
    impl TurnGateway for Fake {
        fn submit(
            &self,
            _: TurnRequest,
        ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
            Box::pin(async { Err(ChannelError::error("unexpected plain submission")) })
        }
        fn submit_with_options(
            &self,
            request: TurnRequest,
            options: ChannelTurnOptions,
        ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
            *self.0.lock().unwrap() = Some((request, options));
            Box::pin(async { Err(ChannelError::error("captured")) })
        }
    }
    #[tokio::test]
    async fn submit_preserves_the_ts_gateway_inputs() {
        let seen = Arc::new(Mutex::new(None));
        let fake = Fake(seen.clone());
        assert!(
            submit(
                &fake,
                "wecom:dm=alice:general".into(),
                None,
                "hello".into(),
                vec![]
            )
            .await
            .is_err()
        );
        let guard = seen.lock().unwrap();
        let (request, options) = guard.as_ref().unwrap();
        let fixture = pilotdeck_protocol::parse_json(include_str!("fixtures/wire.json")).unwrap();
        let JsValue::Array(turns) = fixture.get("turns").unwrap() else {
            panic!("turns")
        };
        let expected = &turns[0];
        assert_eq!(
            Some(&request.session_key),
            expected.get("sessionKey").and_then(JsValue::as_string)
        );
        assert_eq!(
            Some(request.channel_key.as_js()),
            expected.get("channelKey").and_then(JsValue::as_string)
        );
        assert_eq!(
            Some(&request.message),
            expected.get("message").and_then(JsValue::as_string)
        );
        assert_eq!(options.allow_plan_mode_tools, Some(false));
        assert_eq!(
            Some(&JsValue::Array(options.synthetic_messages.clone())),
            expected.get("syntheticMessages")
        );
    }
}
