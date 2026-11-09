//! Deferred hook RPC uses the gateway registry, not session command-hook completion.
use crate::{
    metadata::object,
    runtime::{async_hooks::AsyncHookRegistry, connection::RequestError},
};
use pilotdeck_protocol::JsValue;

pub async fn handle(
    registry: &AsyncHookRegistry,
    params: &JsValue,
) -> Result<JsValue, RequestError> {
    let key = |name: &str| -> Result<pilotdeck_protocol::JsString, RequestError> {
        if matches!(params, JsValue::Null | JsValue::Undefined) {
            let kind = if params.is_undefined() {
                "undefined"
            } else {
                "null"
            };
            return Err(RequestError::new(format!(
                "Cannot read properties of {kind} (reading '{name}')"
            )));
        }
        match params.get(name) {
            Some(JsValue::String(value)) => Ok(value.clone()),
            None | Some(JsValue::Null | JsValue::Undefined) => Ok(Default::default()),
            _ => Err(RequestError::new(format!(
                "input.{name}?.trim is not a function"
            ))),
        }
    };
    let session = key("sessionKey")?;
    // The baseline short-circuits before inspecting invocationId for a blank session.
    let invocation =
        if crate::metadata::js_trim(&String::from_utf16_lossy(session.units())).is_empty() {
            Default::default()
        } else {
            key("invocationId")?
        };
    let result = registry
        .submit(
            session,
            invocation,
            params.get("output").cloned().unwrap_or(JsValue::Undefined),
        )
        .await
        .map_err(|error| {
            let mut reply = RequestError::new(error.message);
            if let Some(module) = error.module_error {
                reply.message = module.message;
                if let Some(code) = module.code {
                    reply.code = String::from_utf16_lossy(code.units());
                }
            }
            reply
        })?;
    Ok(object([
        ("invocationId", JsValue::String(result.invocation_id)),
        ("status", result.status.as_str().into()),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        async_hooks::{AsyncHookRegistration, AsyncHookTurn, TokioHookClock},
        steer::SteerMailbox,
    };
    use std::sync::Arc;
    #[tokio::test]
    async fn rpc_delivers_context_once_and_preserves_session_ownership() {
        let mailbox = SteerMailbox::new(None);
        mailbox.start("run".into());
        let owned = mailbox.clone();
        let registry = AsyncHookRegistry::new(
            Arc::new(TokioHookClock),
            move |session| {
                session.equals_str("session").then(|| AsyncHookTurn {
                    run_id: "run".into(),
                    mailbox: owned.clone(),
                })
            },
            |_, _| {},
        );
        registry
            .register(AsyncHookRegistration {
                session_key: "session".into(),
                invocation_id: "hook".into(),
                hook_name: "test".into(),
                hook_event: "FileChanged".into(),
                timeout_ms: None,
                include_hook_events: false,
            })
            .unwrap();
        let params = |session: &str| {
            object([
                ("sessionKey", session.into()),
                ("invocationId", "hook".into()),
                (
                    "output",
                    object([(
                        "hookSpecificOutput",
                        object([
                            ("hookEventName", "FileChanged".into()),
                            ("additionalContext", "changed".into()),
                        ]),
                    )]),
                ),
            ])
        };
        assert_eq!(
            handle(&registry, &params("other"))
                .await
                .unwrap()
                .get("status"),
            Some(&"unknown".into())
        );
        let owner = params("session");
        let (a, b) = tokio::join!(handle(&registry, &owner), handle(&registry, &owner));
        assert_eq!(a.unwrap().get("status"), Some(&"delivered".into()));
        assert_eq!(b.unwrap().get("status"), Some(&"duplicate".into()));
        assert_eq!(mailbox.drain(&"run".into()).len(), 1);
        assert_eq!(
            handle(&registry, &object([])).await.unwrap_err().code,
            "INVALID_ASYNC_HOOK_RESULT"
        );
    }
}
