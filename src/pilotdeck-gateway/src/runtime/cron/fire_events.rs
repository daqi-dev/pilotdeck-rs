//! Per-event CronFire behavior; scheduler remains the #2381 implementation.
use super::*;

pub const NATIVE_CRON_MAX_ASSISTANT_TEXT_UNITS: usize = 4 * 1024 * 1024;

pub type CronResultDeliveryHandler = Arc<dyn Fn(&JsValue) -> Result<(), CronError> + Send + Sync>;
pub type CronPhaseEventHandler = Arc<dyn Fn(&JsValue) -> Result<(), CronError> + Send + Sync>;
pub type CronTurnEventHandler =
    Arc<dyn Fn(&JsString, &JsString, &JsValue) -> Result<(), CronError> + Send + Sync>;

/// Optional host callbacks and resource policy. None preserves TS's library
/// accumulation semantics; only native assembly opts into a finite text cap.
#[derive(Clone, Default)]
pub struct CronExecutionCallbacks {
    pub on_result_delivery: Option<CronResultDeliveryHandler>,
    pub on_phase_event: Option<CronPhaseEventHandler>,
    pub on_turn_event: Option<CronTurnEventHandler>,
    pub assistant_text_limit_units: Option<usize>,
}

pub(super) struct RunEvents {
    pub outcome: CronRunOutcome,
    pub error: Option<CronRunError>,
    pub text: Vec<u16>,
    forced: bool,
    abort_requested: bool,
    output_limited: bool,
}
impl Default for RunEvents {
    fn default() -> Self {
        Self {
            outcome: CronRunOutcome::Completed,
            error: None,
            text: Vec::new(),
            forced: false,
            abort_requested: false,
            output_limited: false,
        }
    }
}
impl RunEvents {
    fn fail(&mut self, code: &str, message: JsString, forced: bool) {
        self.outcome = CronRunOutcome::Failed;
        self.error = Some(CronRunError {
            code: code.into(),
            message,
        });
        self.forced = forced;
    }
    fn caught(&mut self, error: CronError) {
        if self.forced {
            return;
        }
        match error.code() {
            // Compatibility with old synchronous ports whose Result carries
            // a terminal error event. Real observed ports report each event.
            Some("turn_timeout") => self.fail("cron_run_timeout", error.message().clone(), true),
            Some("agent_aborted") => {
                self.outcome = CronRunOutcome::Aborted;
                self.error = Some(CronRunError {
                    code: "agent_aborted".into(),
                    message: error.message().clone(),
                });
            }
            code => self.fail(
                code.unwrap_or("cron_run_failed"),
                error.message().clone(),
                false,
            ),
        }
    }
    fn abort(&mut self, gateway: &dyn CronAgentGateway, submit: &CronTurnSubmit, reason: &str) {
        if !self.abort_requested {
            self.abort_requested = true;
            let _ = gateway.abort_turn(
                &submit.session_key,
                Some(&submit.run_id),
                Some(&JsString::from(reason)),
            );
        }
    }
    fn append_text(
        &mut self,
        text: &[u16],
        limit: Option<usize>,
        gateway: &dyn CronAgentGateway,
        submit: &CronTurnSubmit,
    ) {
        if self.output_limited {
            return;
        }
        if limit.is_some_and(|limit| text.len() > limit.saturating_sub(self.text.len())) {
            self.output_limited = true;
            self.fail(
                "cron_output_limit",
                JsString::from("Cron result exceeded the native output limit."),
                true,
            );
            self.abort(gateway, submit, "system:output_limit");
        } else {
            self.text.extend_from_slice(text);
        }
    }
}
impl CronFire {
    pub(super) fn emit_phase(
        &self,
        phase: &str,
        task: &JsValue,
        run_id: &JsString,
        now: f64,
        error: Option<&CronRunError>,
    ) -> Result<(), CronError> {
        let Some(callback) = &self.deps.on_phase_event else {
            return Ok(());
        };
        let mut value = JsObject::new();
        value.insert("phase".into(), JsValue::from(phase));
        value.insert("runId".into(), JsValue::String(run_id.clone()));
        value.insert("taskId".into(), task_field(task, "taskId"));
        value.insert("projectKey".into(), task_field(task, "projectKey"));
        value.insert("timestamp".into(), value_string(iso_from_epoch_ms(now)));
        value.insert(
            "title".into(),
            JsValue::String(title(&task_string(task, "message").unwrap_or_default())),
        );
        if let Some(error) = error {
            value.insert("error".into(), error.as_value());
        }
        callback(&JsValue::Object(value))
    }
    pub(super) fn drive_events(
        &self,
        task: &JsValue,
        run_id: &JsString,
        started: f64,
        submit: &CronTurnSubmit,
    ) -> RunEvents {
        let mut state = RunEvents::default();
        if let Err(error) = self.emit_phase("cron_started", task, run_id, started, None) {
            state.caught(error);
            return state;
        }
        let gateway = self.deps.gateway.as_ref();
        let result = gateway.submit_turn_observed(submit, &mut |event| {
            self.deps.store.append_run_event(run_id, event)?;
            if let Some(callback) = &self.deps.on_turn_event
                && let Err(error) = callback(&submit.session_key, &submit.channel_key, event)
            {
                let mut meta = JsObject::new();
                meta.insert("taskId".into(), task_field(task, "taskId"));
                meta.insert("runId".into(), task_field(task, "lastRunId"));
                meta.insert("error".into(), JsValue::String(error.message().clone()));
                self.deps
                    .logger
                    .warn("cron turn event delivery failed", &meta);
            }
            if event_type(event, "assistant_text_delta") {
                match event.get("text") {
                    Some(JsValue::String(text)) => state.append_text(
                        text.units(),
                        self.deps.assistant_text_limit_units,
                        gateway,
                        submit,
                    ),
                    Some(text) => state.append_text(
                        to_display(text)?.units(),
                        self.deps.assistant_text_limit_units,
                        gateway,
                        submit,
                    ),
                    None => state.append_text(
                        JsString::from("undefined").units(),
                        self.deps.assistant_text_limit_units,
                        gateway,
                        submit,
                    ),
                }
            }
            // Native safety failure remains authoritative while the aborted
            // producer drains; the uncapped library path follows TS ordering.
            if state.output_limited {
                return Ok(());
            }
            if event_type(event, "elicitation_request") || event_type(event, "permission_request") {
                let kind = if event_type(event, "permission_request") {
                    "permission_request"
                } else {
                    "elicitation_request"
                };
                state.fail(
                    "cron_interaction_required",
                    JsString::from(format!(
                        "Cron run requested unsupported user interaction through {kind}."
                    )),
                    true,
                );
                state.abort(gateway, submit, "system:interaction_required");
                return Ok(());
            }
            if event_type(event, "error") {
                let code = event.get("code").and_then(JsValue::as_string);
                let message = event
                    .get("message")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap_or_default();
                if code.is_some_and(|code| code.equals_str("turn_timeout")) {
                    state.fail("cron_run_timeout", message, true);
                } else if !state.forced {
                    state.outcome = if code.is_some_and(|code| code.equals_str("agent_aborted")) {
                        CronRunOutcome::Aborted
                    } else {
                        CronRunOutcome::Failed
                    };
                    state.error = Some(CronRunError {
                        code: code.cloned().unwrap_or_else(|| "cron_run_failed".into()),
                        message,
                    });
                }
            }
            Ok(())
        });
        if let Err(error) = result {
            state.caught(error);
        }
        state
    }
}
fn event_type(event: &JsValue, expected: &str) -> bool {
    event
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|kind| kind.equals_str(expected))
}
fn space(unit: &u16) -> bool {
    matches!(*unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
pub(super) fn trim(text: &[u16]) -> JsString {
    let start = text
        .iter()
        .position(|unit| !space(unit))
        .unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|unit| !space(unit))
        .map_or(start, |at| at + 1);
    JsString::from_utf16(text[start..end].to_vec())
}
fn title(message: &JsString) -> JsString {
    let units = message.units();
    let start = units
        .iter()
        .position(|unit| !space(unit))
        .unwrap_or(units.len());
    let units = &units[start..];
    let end = units
        .iter()
        .position(|unit| *unit == u16::from(b'\n'))
        .unwrap_or(units.len());
    let trimmed = trim(&units[..end]);
    JsString::from_utf16(trimmed.units()[..trimmed.units().len().min(120)].to_vec())
}
