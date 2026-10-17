//! One transport replacement; execution state and callback ownership stay put.
use super::*;

pub(super) fn observe(handler: &Arc<dyn HostHandler>, observation: ReconnectObservation) {
    // TS client:1135-1140: telemetry has no authority over the turn result.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handler.observe_transport(observation);
    }));
}

impl HostClient {
    pub(super) async fn reconnect(
        &mut self,
        progress: &Progress,
        handler: &Arc<dyn HostHandler>,
        mut interruption: HostError,
    ) -> Result<(), HostError> {
        // TS client:895-928. No retry before accepted, without all three
        // capabilities/transport opt-in, or after the single replacement.
        if !matches!(
            interruption.kind,
            HostErrorKind::Io | HostErrorKind::Disconnected
        ) || self.reconnect_attempted
            || !self.ack_enabled
            || self.reconnect_command.is_none()
            || progress.stream_id.is_none()
        {
            return Err(interruption);
        }
        self.reconnect_attempted = true;
        let cursor = f64::from_bits(progress.last_applied_sequence.load(Ordering::SeqCst));
        observe(
            handler,
            ReconnectObservation::Started {
                attempt: 1,
                last_applied_sequence: cursor,
            },
        );
        append_diagnostic(&mut interruption, &self.connection);
        // Keep one budget for retained events, in-flight callbacks, cached
        // responses and both generations of transport, never reset on resume.
        let budget = self.connection.budget.clone();
        self.connection.close().await?;
        let command = self.reconnect_command.as_mut().expect("checked command");
        self.connection = Connection::spawn_with_budget(command, self.limits, budget)?;
        let result = timeout(self.limits.handshake_timeout, async {
            let (binding, ack, cancel) =
                handshake(&mut self.connection, None, Some((&self.binding, handler))).await?;
            // TS client:848-860: send the OLD connection's binding, not the
            // replacement generation; the accepted stream identity is stable.
            let mut previous = JsObject::new();
            previous.insert(
                "moduleInstanceId".into(),
                JsValue::String(self.binding.module_instance_id.clone()),
            );
            previous.insert(
                "connectionGeneration".into(),
                JsValue::String(self.binding.connection_generation.clone()),
            );
            let id = JsString::from("resume-1");
            let mut resume = JsObject::new();
            resume.insert("kind".into(), "request".into());
            resume.insert("messageId".into(), JsValue::String(id.clone()));
            resume.insert("method".into(), "resume".into());
            resume.insert(
                "streamId".into(),
                JsValue::String(progress.stream_id.clone().expect("accepted")),
            );
            resume.insert("previousBinding".into(), JsValue::Object(previous));
            resume.insert("lastAppliedSequence".into(), JsValue::Number(cursor));
            self.connection.send(&JsValue::Object(resume))?;
            let raw = self.connection.receive().await?;
            let MessageRef::Response(response) = decode(&raw)? else {
                return Err(HostError::protocol(
                    "Sidecar sent a non-response before resume completed.",
                ));
            };
            // TS client:863-876: success is the validated resume response,
            // independent of whether subsequent replay reaches a terminal.
            if response.in_reply_to != &id {
                return Err(HostError::protocol(
                    "Sidecar resume response does not match the active resume request.",
                ));
            }
            if !response.ok {
                let reason = response
                    .fields
                    .get("code")
                    .and_then(JsValue::as_string)
                    .or_else(|| {
                        response
                            .fields
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(JsValue::as_string)
                    })
                    .map_or_else(
                        || "unknown error".to_owned(),
                        |s| String::from_utf16_lossy(s.units()),
                    );
                return Err(HostError::protocol(format!(
                    "Sidecar resume failed: {reason}"
                )));
            }
            if response.fields.get("streamId").is_some()
                && response.fields.get("streamId").and_then(JsValue::as_string)
                    != progress.stream_id.as_ref()
            {
                return Err(HostError::protocol(
                    "Sidecar resume response changed the stream identity.",
                ));
            }
            self.binding = binding;
            self.ack_enabled = ack;
            self.cancel_enabled = cancel;
            self.reconnect_succeeded = true;
            observe(handler, ReconnectObservation::Succeeded { attempt: 1 });
            Ok(())
        })
        .await
        .unwrap_or_else(|_| {
            Err(HostError::new(
                HostErrorKind::Timeout,
                "Sidecar resume handshake timed out.",
            ))
        });
        result.map_err(|mut error| {
            error
                .message
                .push_str(&format!("; original interruption: {interruption}"));
            error
        })
    }
}

/// During replacement the server can replay an overlap with events already
/// queued/applied here. Deduplicate only on this connection; normal delivery
/// remains strict (TS client:879-892). This overlap tolerance is the explicit
/// A6.1-pr3 requirement, beyond TS's strict event sequence check.
#[derive(Default)]
pub(super) struct ReplayOrder {
    active: bool,
    last: Option<f64>,
}
impl ReplayOrder {
    pub(super) fn begin(&mut self) {
        self.active = true;
        self.last = None;
    }
    /// True means this event is new; false means an ordered duplicate.
    pub(super) fn accept(&mut self, sequence: f64, next: &mut f64) -> Result<bool, HostError> {
        if sequence > *next
            || (!self.active && sequence != *next)
            || (self.active
                && self
                    .last
                    .is_some_and(|last| sequence < last || sequence > last + 1.0))
        {
            return Err(HostError::protocol(format!(
                "Sidecar event sequence mismatch: expected {next}, got {sequence}."
            )));
        }
        self.last = Some(sequence);
        if sequence < *next {
            return Ok(false);
        }
        *next += 1.0;
        Ok(true)
    }
}
