//! `src/adapters/channel/webhook/webhook-render.ts` (baseline cc7c4e6).
use pilotdeck_protocol::JsValue;

use super::status::{is_visible_failure_agent_status_event, visible_status_message};

const VISIBLE_FAILURE_FALLBACK: &str =
    "Agent execution stopped before producing a complete response.";

/// `renderWebhookEvent`: the reply fragment for one gateway event, `None` for
/// the events the renderer does not show.
pub(super) fn render_webhook_event(event: &JsValue) -> Option<String> {
    let kind = event.get("type").and_then(JsValue::as_string)?;
    let kind = String::from_utf16_lossy(kind.units());
    match kind.as_str() {
        "assistant_text_delta" => event
            .get("text")
            .and_then(JsValue::as_string)
            .map(|text| String::from_utf16_lossy(text.units())),
        // Matches the baseline: `webhook-render.ts` (sha256 `ce1c9a51…`, equal
        // to `oracle.json`'s `sourceSha256` for this file at baseline
        // `cc7c4e6`) returns the empty string `""` for this event, not
        // `event.text`, so the event's fields are never read.
        "assistant_thinking_delta" => Some(String::new()),
        "tool_call_started" => Some(format!(
            "\n[{} running]\n",
            super::value_to_display_string(
                &event.get("name").cloned().unwrap_or(JsValue::Undefined)
            )
        )),
        "tool_call_finished" => {
            let tool = event
                .get("toolName")
                .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
                .or_else(|| event.get("toolCallId"))
                .cloned()
                .unwrap_or(JsValue::Undefined);
            let ok = js_truthy_value(&event.get("ok").cloned().unwrap_or(JsValue::Undefined));
            Some(format!(
                "\n[{} {}]\n",
                super::value_to_display_string(&tool),
                if ok { "done" } else { "failed" }
            ))
        }
        "agent_status" => {
            if is_visible_failure_agent_status_event(event) {
                Some(format!(
                    "\n⚠️ {}\n",
                    visible_status_message(event.get("detail"), VISIBLE_FAILURE_FALLBACK)
                ))
            } else {
                None
            }
        }
        "error" => Some(format!(
            "\nError: {}\n",
            super::value_to_display_string(
                &event.get("message").cloned().unwrap_or(JsValue::Undefined)
            )
        )),
        _ => None,
    }
}

/// The truthiness the renderer's `event.ok` needs (any JS truthy value).
fn js_truthy_value(value: &JsValue) -> bool {
    crate::channel::loader::js_truthy(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{JsObject, JsString};

    fn event(kind: &str, fields: &[(&str, JsValue)]) -> JsValue {
        let mut object = JsObject::new();
        object.insert("type".into(), JsValue::String(JsString::from(kind)));
        for (name, value) in fields {
            object.insert((*name).into(), value.clone());
        }
        JsValue::Object(object)
    }
    fn text(value: &str) -> JsValue {
        JsValue::String(JsString::from(value))
    }

    #[test]
    fn renders_the_shown_event_families() {
        assert_eq!(
            render_webhook_event(&event("assistant_text_delta", &[("text", text("hi"))])),
            Some("hi".into()),
            "#1334 text delta"
        );
        assert_eq!(
            render_webhook_event(&event("assistant_thinking_delta", &[])),
            Some("".into()),
            "#1334 thinking is shown empty"
        );
        assert_eq!(
            render_webhook_event(&event("tool_call_started", &[("name", text("web_search"))])),
            Some("\n[web_search running]\n".into()),
            "#1334 tool start"
        );
        assert_eq!(
            render_webhook_event(&event(
                "tool_call_finished",
                &[("toolCallId", text("call_1")), ("ok", JsValue::Bool(true))]
            )),
            Some("\n[call_1 done]\n".into()),
            "#1334 toolName falls back to toolCallId"
        );
        assert_eq!(
            render_webhook_event(&event(
                "tool_call_finished",
                &[("toolName", text("bash")), ("ok", JsValue::Number(0.0))]
            )),
            Some("\n[bash failed]\n".into()),
            "#1334 ok uses JS truthiness"
        );
        assert_eq!(
            render_webhook_event(&event("error", &[("message", text("boom"))])),
            Some("\nError: boom\n".into()),
            "#1334 error event"
        );
        assert_eq!(
            render_webhook_event(&event("something_else", &[])),
            None,
            "#1334 unknown event"
        );
        // A missing text field returns undefined exactly like TS.
        assert_eq!(
            render_webhook_event(&event("assistant_text_delta", &[])),
            None
        );
        // Missing names stringify as `undefined`, like the TS template.
        assert_eq!(
            render_webhook_event(&event("tool_call_started", &[])),
            Some("\n[undefined running]\n".into())
        );
    }

    #[test]
    fn agent_status_renders_only_visible_failures() {
        let detail = {
            let mut detail = JsObject::new();
            detail.insert("severity".into(), text("error"));
            detail.insert("message".into(), text("stopped early"));
            JsValue::Object(detail)
        };
        let visible = event(
            "agent_status",
            &[("event", text("turn_failed")), ("detail", detail)],
        );
        assert_eq!(
            render_webhook_event(&visible),
            Some("\n⚠️ stopped early\n".into()),
            "#1334 visible failure by event name"
        );
        let mut unnamed = JsObject::new();
        unnamed.insert("type".into(), text("agent_status"));
        unnamed.insert("event".into(), text("quiet_progress"));
        assert_eq!(
            render_webhook_event(&JsValue::Object(unnamed.clone())),
            None,
            "#1334 non-failure status is not shown"
        );
        // In the event set without a detail: visible via the set, fallback message.
        let set_only = event("agent_status", &[("event", text("session_busy"))]);
        assert_eq!(
            render_webhook_event(&set_only),
            Some(format!("\n⚠️ {VISIBLE_FAILURE_FALLBACK}\n")),
            "#1334 event set without detail uses the fallback message"
        );
        // Hidden by visible:false even though the event name is in the set.
        let mut hidden_detail = JsObject::new();
        hidden_detail.insert("visible".into(), JsValue::Bool(false));
        let hidden = event(
            "agent_status",
            &[
                ("event", text("session_busy")),
                ("detail", JsValue::Object(hidden_detail)),
            ],
        );
        assert_eq!(
            render_webhook_event(&hidden),
            None,
            "#1334 visible false hides"
        );
        // Not in the set but a detail with error severity: visible via the detail.
        let mut detail = JsObject::new();
        detail.insert("severity".into(), text("error"));
        let by_detail = event(
            "agent_status",
            &[
                ("event", text("not_in_set")),
                ("detail", JsValue::Object(detail)),
            ],
        );
        assert_eq!(
            render_webhook_event(&by_detail),
            Some(format!("\n⚠️ {VISIBLE_FAILURE_FALLBACK}\n")),
            "#1334 error-severity detail renders with fallback message"
        );
    }
}
