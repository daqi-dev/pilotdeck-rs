//! Single text turn, with explicit transport closure and no recovery promise.
use super::{
    ControlError, JsValue, NETWORK_TIMEOUT, Options, Result, Socket, encode, input_value, object,
    open_authenticated, redact, settle, string_is,
};
use futures_util::{SinkExt, StreamExt};
use std::{io, sync::mpsc, time::Duration};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::tungstenite::Message;

const MAX_LINE: usize = 8 * 1024 * 1024;
const OUTPUT_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn input(options: &Options) -> Result<JsValue> {
    let value = input_value(options)?;
    let fields = value.as_object().expect("input_value checks object");
    if fields.entries().iter().any(|(key, _)| {
        ![
            "sessionKey",
            "runId",
            "channelKey",
            "message",
            "projectKey",
            "canPrompt",
        ]
        .iter()
        .any(|allowed| key.equals_str(allowed))
    }) {
        return Err(ControlError::new(
            "input_error",
            "Unsupported stream input field.",
        ));
    }
    for name in ["sessionKey", "runId", "channelKey"] {
        if value
            .get(name)
            .and_then(JsValue::as_string)
            .is_none_or(|s| s.is_empty())
        {
            return Err(ControlError::new(
                "input_error",
                "Stream requires nonempty sessionKey, runId, and channelKey strings.",
            ));
        }
    }
    if value.get("message").and_then(JsValue::as_string).is_none()
        || value.get("canPrompt").and_then(JsValue::as_bool) != Some(false)
        || value
            .get("projectKey")
            .is_some_and(|v| v.as_string().is_none())
    {
        return Err(ControlError::new(
            "input_error",
            "Stream requires a message string, canPrompt:false, and an optional projectKey string.",
        ));
    }
    Ok(value)
}

type Delivery = (String, tokio::sync::oneshot::Sender<io::Result<()>>);
struct Output(mpsc::SyncSender<Delivery>);
impl Output {
    fn new() -> Result<Self> {
        let (send, receive) = mpsc::sync_channel::<Delivery>(1);
        // No runtime blocking task or join: a stalled consumer cannot keep the
        // process alive after the asynchronous output deadline has expired.
        std::thread::Builder::new()
            .name("control-stream-output".into())
            .spawn(move || {
                while let Ok((text, done)) = receive.recv() {
                    let result = super::write_output(&text);
                    let failed = result.is_err();
                    let _ = done.send(result);
                    if failed {
                        break;
                    }
                }
            })
            .map_err(|_| output_error())?;
        Ok(Self(send))
    }
    async fn deliver(&self, value: &JsValue) -> Result<()> {
        let raw = encode(value)?;
        if raw.len() >= MAX_LINE {
            return Err(ControlError::new(
                "output_error",
                "Stream JSON line exceeds 8 MiB; output is incomplete and closing may abort the session.",
            ));
        }
        let (done, delivered) = tokio::sync::oneshot::channel();
        self.0.try_send((raw, done)).map_err(|_| output_error())?;
        match timeout(OUTPUT_TIMEOUT, delivered).await {
            Ok(Ok(Ok(()))) => Ok(()),
            _ => Err(output_error()),
        }
    }
}
fn output_error() -> ControlError {
    ControlError::new(
        "output_error",
        "Could not deliver stream output within 10 seconds; the outcome is unknown and closing may abort the session.",
    )
}
fn disconnected() -> ControlError {
    ControlError::new(
        "transport_error",
        "Gateway stream closed before final; the outcome is unknown and closing may abort the session.",
    )
}

pub(super) async fn run(options: &Options, token: &str, params: JsValue) -> Result<()> {
    let output = Output::new()?;
    let deadline = Instant::now() + NETWORK_TIMEOUT;
    let id = uuid::Uuid::new_v4().to_string();
    let mut socket = timeout_at(deadline, async {
        let mut socket = open_authenticated(options, token, deadline).await?;
        let request = encode(&object(&[
            ("type", "request".into()),
            ("id", id.as_str().into()),
            ("method", "submit_turn".into()),
            ("params", params),
        ]))?;
        if Instant::now() >= deadline {
            return Err(ControlError::new(
                "transport_error",
                "Gateway handshake deadline exceeded before submit.",
            ));
        }
        socket
            .send(Message::Text(request.into()))
            .await
            .map_err(|_| disconnected())?;
        if Instant::now() >= deadline {
            return Err(ControlError::new(
                "transport_error",
                "Gateway submit exceeded its deadline; the outcome may be unknown.",
            ));
        }
        Ok(socket)
    })
    .await
    .map_err(|_| {
        ControlError::new(
            "transport_error",
            "Gateway stream handshake or submit timed out; the outcome may be unknown.",
        )
    })??;
    // An agent turn has no RPC-length total deadline. Every output write has
    // its own budget. There is one frame in delivery and no growing queue.
    let result = receive(&mut socket, &id, token, &output).await;
    settle(
        result,
        async {
            let _ = socket.close(None).await;
        },
        Duration::from_secs(1),
    )
    .await
}

async fn receive(socket: &mut Socket, id: &str, token: &str, output: &Output) -> Result<()> {
    let mut expected = 0_u64;
    let mut last_error = None;
    loop {
        let frame = socket
            .next()
            .await
            .ok_or_else(disconnected)?
            .map_err(|_| disconnected())?;
        if frame.is_close() {
            return Err(disconnected());
        }
        if !frame.is_text() {
            timeout(NETWORK_TIMEOUT, socket.flush())
                .await
                .map_err(|_| disconnected())?
                .map_err(|_| disconnected())?;
            continue;
        }
        let value = pilotdeck_protocol::parse_json(
            frame
                .to_text()
                .map_err(|_| ControlError::new("protocol_error", "Invalid stream frame."))?,
        )
        .map_err(|_| ControlError::new("protocol_error", "Invalid stream JSON."))?;
        if !string_is(&value, "id", id) {
            continue;
        }
        if string_is(&value, "type", "response") {
            return if value.get("ok").and_then(JsValue::as_bool) == Some(false) {
                Err(ControlError {
                    kind: "gateway_error",
                    message: "Gateway rejected the stream.",
                    gateway: value.get("error").map(|v| redact(v, token)),
                })
            } else {
                Err(ControlError::new(
                    "protocol_error",
                    "submit_turn returned a non-stream response.",
                ))
            };
        }
        if !string_is(&value, "type", "event") {
            return Err(ControlError::new(
                "protocol_error",
                "Unexpected frame for this stream.",
            ));
        }
        let valid_sequence = matches!(value.get("seq"), Some(JsValue::Number(n))
            if n.is_finite() && *n >= 0.0 && *n <= 9_007_199_254_740_991.0 && *n == expected as f64);
        if !valid_sequence {
            return Err(ControlError::new(
                "protocol_error",
                "Stream sequence is missing, duplicated, or out of order; output is incomplete.",
            ));
        }
        let final_frame = value
            .get("final")
            .and_then(JsValue::as_bool)
            .ok_or_else(|| {
                ControlError::new("protocol_error", "Stream final flag must be boolean.")
            })?;
        let event = value
            .get("event")
            .filter(|v| {
                v.as_object().is_some() && v.get("type").and_then(JsValue::as_string).is_some()
            })
            .ok_or_else(|| {
                ControlError::new("protocol_error", "Stream event must have a type string.")
            })?;
        let event = redact_event(event, token);
        // Envelope keys and transport identity stay intact even if a short
        // authentication token happens to equal a structural field name.
        output
            .deliver(&object(&[
                ("version", "pilotdeck-control/1".into()),
                ("type", "event".into()),
                ("id", id.into()),
                ("seq", JsValue::Number(expected as f64)),
                ("final", JsValue::Bool(final_frame)),
                ("event", event.clone()),
            ]))
            .await?;
        let original_event = value.get("event").expect("validated event");
        if string_is(original_event, "type", "error") {
            last_error = Some(event.clone());
        }
        if final_frame {
            if let Some(error) = last_error {
                return Err(ControlError {
                    kind: "turn_error",
                    message: "Gateway turn reported an error.",
                    gateway: Some(error),
                });
            }
            if !string_is(original_event, "type", "turn_completed") {
                return Err(ControlError::new(
                    "protocol_error",
                    "Gateway final event is not a known terminal result.",
                ));
            }
            if string_is(original_event, "finishReason", "completed") {
                return Ok(());
            }
            return Err(ControlError {
                kind: "turn_incomplete",
                message: "Gateway turn ended without completed status; inspect the final event.",
                gateway: Some(event),
            });
        }
        expected += 1;
    }
}

// Streaming consumers need stable property names and terminal discriminators.
// These protocol identifiers are public metadata, excluded from echo filtering.
// Other string values follow the ordinary token echo policy, without mutating keys.
fn redact_event(value: &JsValue, token: &str) -> JsValue {
    enum Task<'a> {
        Value(&'a JsValue, bool),
        Array(usize),
        Object(Vec<pilotdeck_protocol::JsString>),
    }
    let secret: Vec<u16> = token.encode_utf16().collect();
    let mut tasks = vec![Task::Value(value, true)];
    let mut values = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Value(JsValue::Object(items), root) => {
                let entries = items.entries();
                tasks.push(Task::Object(
                    entries.iter().map(|(key, _)| key.clone()).collect(),
                ));
                for (key, item) in entries.iter().rev() {
                    if root
                        && item.as_string().is_some()
                        && (key.equals_str("type") || key.equals_str("finishReason"))
                    {
                        // Mark public discriminator strings for verbatim emission.
                        tasks.push(Task::Value(item, true));
                    } else {
                        tasks.push(Task::Value(item, false));
                    }
                }
            }
            Task::Value(JsValue::Array(items), _) => {
                tasks.push(Task::Array(items.len()));
                tasks.extend(items.iter().rev().map(|v| Task::Value(v, false)));
            }
            Task::Value(JsValue::String(text), false) => values.push(JsValue::String(
                pilotdeck_protocol::JsString::from_utf16(super::scrub_units(text.units(), &secret)),
            )),
            Task::Value(other, _) => values.push(other.clone()),
            Task::Array(len) => {
                let children = values.split_off(values.len() - len);
                values.push(JsValue::Array(children));
            }
            Task::Object(keys) => {
                let children = values.split_off(values.len() - keys.len());
                let mut object = pilotdeck_protocol::JsObject::new();
                for (key, child) in keys.into_iter().zip(children) {
                    object.insert(key, child);
                }
                values.push(JsValue::Object(object));
            }
        }
    }
    values.pop().expect("one stream event")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_credential_with_many_small_values_has_bounded_work() {
        let token = "a".repeat(65_535);
        let event = object(&[
            ("type", "structured_output".into()),
            (
                "payload",
                JsValue::Array(vec![JsValue::from("short"); 100_000]),
            ),
        ]);
        let start = std::time::Instant::now();
        let result = redact_event(&event, &token);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "short values must not build a token-length matcher each time"
        );
        assert_eq!(result, event);
    }
}
