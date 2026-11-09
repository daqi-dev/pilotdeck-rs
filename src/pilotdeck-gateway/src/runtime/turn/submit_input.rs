//! Current-turn input projection; history, host policy and upload leases remain host-owned.
use super::{
    routes::control::{ProductionSteerTurnComposer, SteerTurnComposer},
    submit::SubmitError,
};
use crate::{
    config::permission::PermissionMode,
    runtime::permission::{
        DefaultPermissionContextOptions, PermissionContext, create_default_permission_context,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// Borrow actual host/session policy; none of these values is read from wire permissionContext.
#[derive(Default)]
pub(super) struct InputPolicy<'a> {
    pub permission_context: Option<&'a PermissionContext>,
    pub session_mode: Option<&'a JsString>,
    pub session_base_mode: Option<&'a JsString>,
    pub default_mode: Option<&'a JsString>,
    pub skip_permissions: bool,
    pub max_turns: Option<f64>,
    pub max_budget_usd: Option<f64>,
}

/// The unconfigured-host variant. Production hosts with policy use the sibling overload.
#[cfg(test)]
pub(super) async fn prepare_input(
    input: &JsValue,
    cwd: &JsString,
) -> Result<JsObject, SubmitError> {
    prepare_input_with_policy(input, cwd, &InputPolicy::default()).await
}

pub(super) async fn prepare_input_with_policy(
    input: &JsValue,
    cwd: &JsString,
    policy: &InputPolicy<'_>,
) -> Result<JsObject, SubmitError> {
    let cwd_path = cwd.to_utf8().map_err(|_| {
        SubmitError::new(
            "gateway_submit_failed",
            "Working directory is not a native UTF-8 path.",
        )
    })?;
    let prepared = ProductionSteerTurnComposer::new(cwd_path)
        .prepare(input)
        .await
        .map_err(|error| {
            SubmitError::new(
                "gateway_submit_failed",
                String::from_utf16_lossy(error.message.units()),
            )
        })?;
    let mut output = JsObject::new();
    for key in ["allowedTools", "disallowedTools"] {
        if let Some(items) = optional_array(input, key)? {
            if items.iter().any(|item| item.as_string().is_none()) {
                return Err(invalid("Tool restrictions must contain strings."));
            }
            output.insert(key.into(), JsValue::Array(items.to_vec()));
        }
    }
    output.insert(
        "messages".into(),
        JsValue::Array(vec![record([
            ("role", "user".into()),
            ("content", JsValue::Array(prepared.content)),
        ])]),
    );
    output.insert(
        "seedState".into(),
        record([(
            "allowedReadFiles",
            JsValue::Array(
                prepared
                    .allowed_read_files
                    .into_iter()
                    .map(JsValue::String)
                    .collect(),
            ),
        )]),
    );
    if let Some(items) = optional_array(input, "syntheticMessages")? {
        let mut messages = Vec::with_capacity(items.len());
        for item in items {
            let text = item
                .get("text")
                .and_then(JsValue::as_string)
                .ok_or_else(|| invalid("syntheticMessages entries must contain string text."))?;
            let purpose = match item.get("purpose") {
                None | Some(JsValue::Null | JsValue::Undefined) => "channel_hint".into(),
                Some(JsValue::String(value)) => JsValue::String(value.clone()),
                _ => return Err(invalid("syntheticMessages purpose must be a string.")),
            };
            messages.push(record([
                ("role", "user".into()),
                (
                    "content",
                    JsValue::Array(vec![record([
                        ("type", "text".into()),
                        ("text", JsValue::String(text.clone())),
                    ])]),
                ),
                (
                    "metadata",
                    record([("synthetic", JsValue::Bool(true)), ("purpose", purpose)]),
                ),
            ]));
        }
        if !messages.is_empty() {
            output.insert("syntheticMessages".into(), JsValue::Array(messages));
        }
    }
    // Pending refs are explicit output for the host's lease stage, never forged as local paths.
    if let Some(uploads) = optional_array(input, "uploadedAttachments")?
        && !uploads.is_empty()
    {
        if !input
            .get("projectKey")
            .and_then(JsValue::as_string)
            .is_some_and(|key| !key.is_empty())
        {
            return Err(SubmitError::new(
                "PROJECT_NOT_FOUND",
                "projectKey is required for uploaded attachments.",
            ));
        }
        for upload in uploads {
            if !upload
                .get("uploadId")
                .and_then(JsValue::as_string)
                .is_some_and(|id| !id.is_empty())
            {
                return Err(invalid(
                    "uploadedAttachments entries must contain uploadId.",
                ));
            }
            if let Some(ids) = optional_array(upload, "attachmentIds")?
                && ids.iter().any(|id| id.as_string().is_none())
            {
                return Err(invalid("attachmentIds must contain strings."));
            }
        }
        output.insert(
            "uploadedAttachments".into(),
            JsValue::Array(uploads.to_vec()),
        );
    }

    for (key, cap) in [
        ("maxTurns", policy.max_turns),
        ("maxBudgetUsd", policy.max_budget_usd),
    ] {
        let value = match input.get(key) {
            None | Some(JsValue::Undefined) => None,
            Some(JsValue::Number(value))
                if value.is_finite() && (key == "maxTurns" || *value > 0.0) =>
            {
                Some(*value)
            }
            _ if key == "maxBudgetUsd" => {
                return Err(SubmitError::new(
                    "INVALID_MAX_BUDGET_USD",
                    "maxBudgetUsd must be a positive finite number.",
                ));
            }
            _ => return Err(invalid("maxTurns must be a finite number.")),
        };
        if let Some(value) = match (value, cap) {
            (Some(value), Some(cap)) => Some(value.min(cap)),
            (value, None) => value,
            (None, cap) => cap,
        } {
            output.insert(key.into(), JsValue::Number(value));
        }
    }
    for key in ["canPrompt", "canElicit"] {
        if let Some(value) = optional_bool(input, key)? {
            output.insert(key.into(), JsValue::Bool(value));
        }
    }
    let requested_mode = permission_mode(input, "mode", true)?;
    let input_mode = requested_mode.as_ref().or(policy.session_mode);
    let mode = input_mode
        .or(policy.default_mode)
        .cloned()
        .or_else(|| policy.skip_permissions.then(|| "bypassPermissions".into()));
    let base = permission_mode(input, "basePermissionMode", false)?
        .or_else(|| policy.session_base_mode.cloned())
        .or_else(|| {
            requested_mode
                .clone()
                .filter(|mode| !mode.equals_str("plan"))
        })
        .or_else(|| policy.default_mode.cloned());
    if let Some(base) = base {
        output.insert("basePermissionMode".into(), JsValue::String(base));
    }
    let run_mode = match input.get("runMode") {
        None | Some(JsValue::Null | JsValue::Undefined) => None,
        Some(JsValue::String(mode)) if mode.is_empty() => None,
        Some(JsValue::String(mode))
            if ["agent", "plan", "ask"].iter().any(|s| mode.equals_str(s)) =>
        {
            Some(mode.clone())
        }
        _ => Some("agent".into()),
    }
    .unwrap_or_else(|| {
        if input_mode.is_some_and(|mode| mode.equals_str("plan")) {
            "plan".into()
        } else {
            "agent".into()
        }
    });
    output.insert("runMode".into(), JsValue::String(run_mode));
    output.insert(
        "allowPlanModeTools".into(),
        JsValue::Bool(
            optional_bool(input, "allowPlanModeTools")?
                .unwrap_or_else(|| input_mode.is_some_and(|mode| mode.equals_str("plan"))),
        ),
    );
    let mut context = policy.permission_context.cloned().unwrap_or_else(|| {
        create_default_permission_context(DefaultPermissionContextOptions {
            cwd: cwd.clone(),
            ..Default::default()
        })
    });
    context.cwd = cwd.clone();
    if let Some(mode) = mode {
        context.mode = PermissionMode::new(mode);
    }
    if let Some(can_prompt) = optional_bool(input, "canPrompt")? {
        context.can_prompt = can_prompt;
    }
    if context.policy_can_prompt == Some(false) {
        context.can_prompt = false;
    }
    output.insert(
        "permissionMode".into(),
        JsValue::String(context.mode.as_js_string().clone()),
    );
    output.insert("permissionContext".into(), context.to_value());
    Ok(output)
}

fn record<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}

fn invalid(message: &str) -> SubmitError {
    SubmitError::new("gateway_submit_failed", message)
}

fn optional_array<'a>(input: &'a JsValue, key: &str) -> Result<Option<&'a [JsValue]>, SubmitError> {
    match input.get(key) {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(None),
        Some(JsValue::Array(items)) => Ok(Some(items)),
        _ => Err(invalid(&format!("{key} must be an array."))),
    }
}

fn optional_bool(input: &JsValue, key: &str) -> Result<Option<bool>, SubmitError> {
    match input.get(key) {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(None),
        Some(JsValue::Bool(value)) => Ok(Some(*value)),
        _ => Err(invalid(&format!("{key} must be a boolean."))),
    }
}

fn permission_mode(
    input: &JsValue,
    key: &str,
    allow_plan: bool,
) -> Result<Option<JsString>, SubmitError> {
    match input.get(key) {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(None),
        Some(JsValue::String(value)) if value.is_empty() => Ok(None),
        Some(JsValue::String(value))
            if value.equals_str("default")
                || value.equals_str("bypassPermissions")
                || (allow_plan && value.equals_str("plan")) =>
        {
            Ok(Some(value.clone()))
        }
        _ => Err(SubmitError::new(
            "INVALID_PERMISSION_MODE",
            format!("Invalid {key}."),
        )),
    }
}

#[cfg(test)]
#[path = "submit_input_tests.rs"]
mod tests;
