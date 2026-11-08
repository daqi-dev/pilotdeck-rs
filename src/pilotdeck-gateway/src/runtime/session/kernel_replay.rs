//! Reconstruction of the product's bounded active view from committed Kernel
//! events. It performs no execution writes and reuses the normal event projector.
use crate::runtime::{
    project::RuntimeError,
    turn::projector::{
        GatewayAgentEventProjectionInput, GatewayAgentEventProjector,
        GatewayAgentEventProjectorOptions, GatewayToolResultArtifactStore,
    },
};
use pilotdeck_protocol::{JsValue, parse_json};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path, sync::Arc};
fn error(e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::new(e.to_string())
}
fn js(value: &Value) -> Result<JsValue, RuntimeError> {
    parse_json(&value.to_string()).map_err(error)
}
pub(crate) fn snapshot(
    native: &JsValue,
    session: &str,
    home: &Path,
) -> Result<JsValue, RuntimeError> {
    let native = super::kernel::json_value(native)?;
    let run = native["runId"].as_str();
    let active = matches!(
        native["status"].as_str(),
        Some("accepted" | "running" | "needs_human")
    );
    let Some(run) = run else {
        return js(
            &json!({"active":false,"sessionKey":session,"events":[],"nativeExecution":native}),
        );
    };
    let root = native["executionId"].as_str();
    let journal = native["journal"].as_array().cloned().unwrap_or_default();
    let projector = GatewayAgentEventProjector::new(GatewayAgentEventProjectorOptions {
        tool_result_artifacts: Some(Arc::new(
            GatewayToolResultArtifactStore::new(home.join("tool-results"), 4096).map_err(error)?,
        )),
        ..Default::default()
    })
    .map_err(error)?;
    let mut raw = Vec::new();
    let input = native["execution"]["invocation"]["payload"].clone();
    for kind in ["turn_started", "input_accepted", "user_prompt_submitted"] {
        raw.push(json!({"type":kind,"sessionId":session,"turnId":run,"input":input}));
    }
    let mut waits = HashSet::new();
    let mut final_event = None;
    let mut terminal = None;
    let mut failure = false;
    for event in &journal {
        if event["correlation_id"] != run {
            continue;
        }
        if event["state"] == "needs_human"
            && let (Some(wait), Some(call)) = (
                event["payload"]["waiting"]["id"].as_str(),
                event["call_ref"].as_str(),
            )
            && waits.insert(wait.to_string())
        {
            raw.push(json!({"type":"permission_request","requestId":wait,"executionId":call,"toolName":event["payload"]["waiting"]["schema"]["tool_name"],"payload":event["payload"]["waiting"]["schema"]}));
        }
        if event["call_ref"].as_str() != root {
            continue;
        }
        if let Some(agent) = event["payload"]["content"].get("agent_event") {
            let mut agent = agent.clone();
            agent["sessionId"] = json!(session);
            agent["turnId"] = json!(run);
            if agent["type"] == "turn_completed" {
                final_event = Some(agent);
            } else if agent["type"] == "permission_request" {
                if let Some(wait) = agent["requestId"].as_str()
                    && waits.insert(wait.to_string())
                {
                    raw.push(agent);
                }
            } else {
                if agent["type"] == "turn_failed" {
                    failure = true;
                }
                raw.push(agent);
            }
        }
        if event["kind"] == "response" {
            if let Some(text) = event["payload"]["content"].as_str() {
                raw.push(json!({"type":"model_event","sessionId":session,"turnId":run,"event":{"type":"text_delta","text":text}}));
            }
            terminal = Some(event);
        }
    }
    if let Some(event) = terminal {
        let outcome = event["payload"]["outcome"].as_str().unwrap_or("unknown");
        if outcome == "failed" && !failure {
            let original = &event["payload"]["error"];
            let message = original["params"]["detail"]
                .as_str()
                .or_else(|| original["message"].as_str())
                .map(str::to_string)
                .unwrap_or_else(|| original.to_string());
            raw.push(json!({"type":"turn_failed","error":{"code":original["code"].as_str().unwrap_or("KERNEL_EXECUTION_FAILED"),"message":message,"details":{"executionId":root,"kernelError":original}}}));
        }
        if outcome == "unknown" {
            raw.push(json!({"type":"turn_failed","error":{"code":"result_unknown","nativeCode":"KERNEL_OUTCOME_UNKNOWN","message":"Execution stopped waiting; an external effect may still have happened. Inspect the original execution before retrying.","details":{"executionId":root,"outcome":"unknown"}}}));
        }
        let mut final_event=final_event.unwrap_or_else(||json!({"type":"turn_completed","result":{"type":if outcome=="completed"{"success"}else if outcome=="cancelled"{"aborted"}else{"error"},"stopReason":outcome,"usage":{},"permissionDenials":[],"turns":0}}));
        if outcome == "unknown" {
            final_event["result"]["type"] = json!("error");
            final_event["result"]["stopReason"] = json!("kernel_outcome_unknown");
        }
        final_event["status"] = json!(outcome);
        raw.push(final_event);
    }
    let mut frames = Vec::new();
    for mut event in raw {
        event["sessionId"] = json!(session);
        event["turnId"] = json!(run);
        super::kernel_projection::normalize_display_timelines(&mut event, run);
        if event["type"] == "turn_failed" {
            let mut frame = event["error"].clone();
            frame["type"] = json!("error");
            frame["runId"] = json!(run);
            frame["recoverable"] = json!(false);
            frames.push(frame);
            continue;
        }
        if event["type"] == "permission_request" {
            event["runId"] = json!(run);
            frames.push(event);
            continue;
        }
        for frame in projector
            .project(&GatewayAgentEventProjectionInput {
                event: &js(&event)?,
                run_id: run,
                forward_subagent_text: false,
            })
            .map_err(error)?
        {
            frames.push(super::kernel::json_value(&frame)?);
        }
    }
    let truncated = frames.len() > 512;
    if truncated {
        frames.drain(..frames.len() - 512);
    }
    js(
        &json!({"active":active,"sessionKey":session,"runId":run,"events":frames,"terminal":terminal.is_some(),"truncated":truncated,"nativeExecution":native}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kernel_restart_replay_uses_original_root_wait_and_durable_terminal() {
        let home = std::env::temp_dir().join(format!("pd-kernel-replay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let mut native = json!({"runId":"run","executionId":"root","status":"needs_human","execution":{"invocation":{"payload":{"type":"text","text":"hello"}}},"journal":[{"call_ref":"root","correlation_id":"run","state":"needs_human","payload":{"waiting":{"id":"wait","schema":{"tool_name":"Read"}}}},{"call_ref":"root","correlation_id":"run","payload":{"content":{"agent_event":{"type":"model_event","event":{"type":"text_delta","text":"hello"}}}}}]});
        let first =
            super::super::kernel::json_value(&snapshot(&js(&native).unwrap(), "s", &home).unwrap())
                .unwrap();
        assert_eq!(first["active"], true);
        let permission = first["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["requestId"] == "wait")
            .unwrap();
        assert_eq!(permission["executionId"], "root");
        native["status"] = json!("failed");
        native["journal"].as_array_mut().unwrap().push(json!({"kind":"response","call_ref":"root","correlation_id":"run","payload":{"outcome":"failed","error":{"code":"ORIGINAL","params":{"detail":"actual failure"}}}}));
        let final_view =
            super::super::kernel::json_value(&snapshot(&js(&native).unwrap(), "s", &home).unwrap())
                .unwrap();
        assert_eq!(final_view["active"], false);
        assert_eq!(final_view["terminal"], true);
        assert!(
            final_view["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["code"] == "ORIGINAL" || e["message"] == "actual failure")
        );
        native["status"] = json!("unknown");
        native["journal"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap()["payload"]["outcome"] = json!("unknown");
        let unknown =
            super::super::kernel::json_value(&snapshot(&js(&native).unwrap(), "s", &home).unwrap())
                .unwrap();
        let error = unknown["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["code"] == "result_unknown")
            .unwrap();
        assert_eq!(error["nativeCode"], "KERNEL_OUTCOME_UNKNOWN");
        assert_eq!(error["details"]["executionId"], "root");
        std::fs::remove_dir_all(home).unwrap();
    }
}
