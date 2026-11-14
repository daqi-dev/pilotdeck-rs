use super::super::requests::Requests;
use super::input_property;
use crate::{
    js_value,
    runtime::{
        connection::{Call, RequestError},
        turn::read::{self, RunEventsInput},
    },
    storage::RunRef,
};
use pilotdeck_protocol::{JsString, JsValue};

// #3143 fix-r1 (sibling#3): crate-visible for the management test's
// single-source filter, the same visibility `settings::METHODS` carries.
pub(crate) const METHODS: &[&str] = &["run_get", "run_events", "run_reattach"];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::run_request);

fn read_error(error: read::TurnReadError) -> RequestError {
    RequestError {
        code: error
            .code
            .unwrap_or_else(|| "gateway_request_failed".into()),
        message: error.message,
        details: None,
        validation: None,
    }
}

/// `${value}` in the registry's run-key template: ToString including
/// ToPrimitive's failure surfaces (`{toString: null}` → the boundary's
/// `gateway_request_failed` "Cannot convert object to primitive value",
/// deep arrays → "Maximum call stack size exceeded"; #1604 probes
/// `_tmp/20261026-issue-1604/ts-probe/output2.txt` q3/q7).
fn template_text(value: &JsValue) -> Result<JsString, RequestError> {
    js_value::to_string_units_checked(value)
        .map(JsString::from_utf16)
        .map_err(|error| RequestError::new(error.message()))
}

/// The run reference, read in the TS key template's own order
/// (RunRegistry.ts:70: `` `${projectKey ?? ""}\0${sessionKey}\0${runId}` ``):
/// `projectKey` folds nullish to the empty project and everything else
/// coerces; `sessionKey` / `runId` always coerce, a missing field reading as
/// the literal "undefined" (probe q1: `runGet({})` hits a record registered
/// under those literal strings). TS has no input guard here — the coercion
/// lives inside `registry.get`, so this replaces the old
/// "`sessionKey must be a string.`" self-invented errors (#1604).
fn run_ref(params: &JsValue) -> Result<RunRef, RequestError> {
    let project_key = match input_property(params, "projectKey")? {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => None,
        Some(value) => Some(template_text(&value)?),
    };
    Ok(RunRef {
        project_key,
        session_key: template_text(
            &input_property(params, "sessionKey")?.unwrap_or(JsValue::Undefined),
        )?,
        run_id: template_text(&input_property(params, "runId")?.unwrap_or(JsValue::Undefined))?,
    })
}

impl Requests {
    pub(in crate::native_process) fn run_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            // The capability gate precedes every input read (probe r11/r12:
            // an unconfigured registry answers CAPABILITY_UNAVAILABLE even
            // for a params shape whose fields would throw on read).
            let registry: Option<&dyn read::TurnRunReader> = self
                .runs
                .as_deref()
                .map(|runs| runs as &dyn read::TurnRunReader);
            let Some(registry) = registry else {
                return Err(read_error(read::registry_unavailable()));
            };
            if method == "run_events" {
                // Cursor and limit validate before the key template's reads
                // (InProcessGateway.ts:2533-2538; probe r7: an invalid cursor
                // wins over a sessionKey whose ToString throws, and nullish
                // params throws reading 'afterSeq' — probe r10).
                let after_seq = input_property(&params, "afterSeq")?;
                let limit = input_property(&params, "limit")?;
                if let Some(error) = read::cursor_error(after_seq.as_ref(), limit.as_ref()) {
                    return Err(read_error(error));
                }
                read::run_events(
                    Some(registry),
                    &RunEventsInput {
                        run: run_ref(&params)?,
                        after_seq,
                        limit,
                    },
                )
                .await
                .map_err(read_error)
            } else {
                let run = run_ref(&params)?;
                if method == "run_reattach" {
                    read::run_reattach(Some(registry), &run)
                        .await
                        .map(|v| v.unwrap_or(JsValue::Null))
                        .map_err(read_error)
                } else {
                    read::run_get(Some(registry), &run)
                        .await
                        .map(|v| v.unwrap_or(JsValue::Null))
                        .map_err(read_error)
                }
            }
        })
    }
}
