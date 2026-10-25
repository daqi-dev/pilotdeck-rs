//! #686: user-visible notice for pending interaction items discarded at the
//! end of a turn. Discarding remains the decided behaviour; this module only
//! makes it observable. The notice is emitted twice:
//!
//! * immediately as one structured stderr log line, and
//! * as a `warning` AgentEvent injected after the next turn's `turn_started`
//!   (the projector already maps `warning` to a user-visible `agent_status`).
//!
//! A notice is scoped to one session and carries the discarded run identity.
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

/// Event name used both as the log event and as the `warning.code`.
pub const DISCARD_NOTICE_EVENT: &str = "interaction_pending_discarded";

/// Where the discarded pending items came from. `Interaction` is reserved for
/// an elicitation/user-dialog port; the current turn assembly installs none, so
/// production does not create that source today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardSource {
    Hooks,
    Interaction,
    Steer,
    Permission,
}
impl DiscardSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hooks => "hooks",
            Self::Interaction => "interaction",
            Self::Steer => "steer",
            Self::Permission => "permission",
        }
    }
}
/// One aggregate notice: "the previous turn had `count` pending items of
/// `source` discarded". `run_id` is the discarded run/turn identity (the
/// gateway defaults `runId` to `turnId`, `runtime/turn/live.rs:790-791`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscardNotice {
    pub source: DiscardSource,
    pub count: usize,
    pub session_id: JsString,
    pub run_id: JsString,
}
impl DiscardNotice {
    pub fn new(
        source: DiscardSource,
        count: usize,
        session_id: JsString,
        run_id: JsString,
    ) -> Self {
        Self {
            source,
            count,
            session_id,
            run_id,
        }
    }
    pub fn message(&self) -> String {
        format!(
            "Previous turn had {} pending {} item(s) discarded.",
            self.count,
            self.source.as_str()
        )
    }
    /// Structured log/event record. Field order is part of the locked shape.
    pub fn to_event(&self) -> JsObject {
        let mut event = JsObject::new();
        event.insert("event".into(), DISCARD_NOTICE_EVENT.into());
        event.insert("source".into(), self.source.as_str().into());
        event.insert("count".into(), JsValue::Number(self.count as f64));
        event.insert("sessionId".into(), JsValue::String(self.session_id.clone()));
        event.insert("runId".into(), JsValue::String(self.run_id.clone()));
        event
    }
    pub fn log_line(&self) -> String {
        stringify_json(&JsValue::Object(self.to_event()))
            .unwrap_or(None)
            .unwrap_or_default()
    }
    /// Immediate log outlet. This is intentionally a plain stderr line: the
    /// gateway has no tracing subscriber, and a log line must not be lost just
    /// because no next turn binds this session again.
    pub fn log(&self) {
        eprintln!("{}", self.log_line());
    }
    /// Next-turn visible outlet. The active turn identity satisfies the event
    /// sink's identity gate; the discarded identity stays in `metadata`.
    pub fn to_warning(&self, active_session: &JsString, active_turn: &JsString) -> JsObject {
        let mut metadata = JsObject::new();
        metadata.insert("source".into(), self.source.as_str().into());
        metadata.insert("count".into(), JsValue::Number(self.count as f64));
        metadata.insert("sessionId".into(), JsValue::String(self.session_id.clone()));
        metadata.insert("runId".into(), JsValue::String(self.run_id.clone()));
        let mut warning = JsObject::new();
        warning.insert("type".into(), "warning".into());
        warning.insert("sessionId".into(), JsValue::String(active_session.clone()));
        warning.insert("turnId".into(), JsValue::String(active_turn.clone()));
        warning.insert("code".into(), DISCARD_NOTICE_EVENT.into());
        warning.insert("message".into(), JsValue::String(self.message().into()));
        warning.insert("metadata".into(), JsValue::Object(metadata));
        warning
    }
}
/// Remove only this session's notices, preserving other sessions.
pub fn take_session_notices(
    notices: &mut Vec<DiscardNotice>,
    session: &JsString,
) -> Vec<DiscardNotice> {
    let mut taken = Vec::new();
    notices.retain(|notice| {
        if &notice.session_id == session {
            taken.push(notice.clone());
            false
        } else {
            true
        }
    });
    taken
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice() -> DiscardNotice {
        DiscardNotice::new(DiscardSource::Hooks, 2, "s".into(), "run".into())
    }

    #[test]
    fn notice_shape_is_stable() {
        let notice = notice();
        assert_eq!(
            notice.log_line(),
            r#"{"event":"interaction_pending_discarded","source":"hooks","count":2,"sessionId":"s","runId":"run"}"#
        );
        let warning = JsValue::Object(notice.to_warning(&"s2".into(), &"t2".into()));
        assert_eq!(
            stringify_json(&warning).unwrap().unwrap(),
            r#"{"type":"warning","sessionId":"s2","turnId":"t2","code":"interaction_pending_discarded","message":"Previous turn had 2 pending hooks item(s) discarded.","metadata":{"source":"hooks","count":2,"sessionId":"s","runId":"run"}}"#
        );
        assert_eq!(
            notice.message(),
            "Previous turn had 2 pending hooks item(s) discarded."
        );
    }

    #[test]
    fn ts_oracle_locks_the_no_notice_baseline_for_hooks_and_permission() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/interaction-discard/oracle.json"
        ))
        .unwrap();
        let hooks = oracle.get("hooks").unwrap();
        assert_eq!(hooks.get("noticeEvents").unwrap(), &JsValue::Array(vec![]));
        let permission = oracle.get("permission").unwrap();
        assert_eq!(
            permission.get("noticeEvents").unwrap(),
            &JsValue::Array(vec![])
        );
        let steer = oracle.get("steer").unwrap();
        let notice = steer.get("noticeEvents").unwrap();
        let JsValue::Array(items) = notice else {
            panic!()
        };
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0]
                .get("type")
                .and_then(JsValue::as_string)
                .map(|v| v.to_utf8().unwrap()),
            Some("steer_unapplied".into())
        );
        assert_eq!(
            items[0]
                .get("reason")
                .and_then(JsValue::as_string)
                .map(|v| v.to_utf8().unwrap()),
            Some("turn_ended".into())
        );
    }

    #[test]
    fn take_session_notices_does_not_leak_across_sessions() {
        let mut notices = vec![
            DiscardNotice::new(DiscardSource::Hooks, 1, "a".into(), "r1".into()),
            DiscardNotice::new(DiscardSource::Permission, 3, "b".into(), "r2".into()),
            DiscardNotice::new(DiscardSource::Steer, 2, "a".into(), "r3".into()),
        ];
        let taken = take_session_notices(&mut notices, &"a".into());
        assert_eq!(taken.len(), 2);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].session_id, "b".into());
    }
}
