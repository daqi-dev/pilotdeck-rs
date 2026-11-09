use super::scheduler::{runtime_error, string};
use super::{CronRuntime, NativeTurnRequest, RuntimeError};
use crate::cron::{schedule::iso, tasks::CronStore};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Outcome {
    outcome: &'static str,
    error: Option<Value>,
    synthetic_abort_error: bool,
    forced: bool,
    abort_requested: bool,
    text: String,
    events: usize,
}
impl Default for Outcome {
    fn default() -> Self {
        Self {
            outcome: "completed",
            error: None,
            synthetic_abort_error: false,
            forced: false,
            abort_requested: false,
            text: String::new(),
            events: 0,
        }
    }
}
impl Outcome {
    fn fail(&mut self, code: &str, message: &str, forced: bool) {
        if self.forced && !forced {
            return;
        }
        self.outcome = if code == "agent_aborted" {
            "aborted"
        } else {
            "failed"
        };
        self.error = Some(json!({"code":code,"message":message}));
        self.synthetic_abort_error = false;
        self.forced = forced;
    }
}

pub(super) async fn execute(
    runtime: &Arc<CronRuntime>,
    store: CronStore,
    task: Value,
    run_id: String,
) -> Result<(), RuntimeError> {
    let started = string(&task, "updatedAt").to_owned();
    let session = string(&task, "sessionKey").to_owned();
    let channel = string(&task, "channelKey").to_owned();
    let title: String = string(&task, "message")
        .trim_start()
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(120)
        .collect();
    phase(
        runtime,
        &task,
        &run_id,
        "cron_started",
        &started,
        &title,
        None,
    );
    let outcome = Arc::new(Mutex::new(Outcome::default()));
    let sink_state = outcome.clone();
    let sink_store = store.clone();
    let sink_runtime = runtime.clone();
    let sink_run = run_id.clone();
    let sink_session = session.clone();
    let sink_channel = channel.clone();
    let abort_jobs = Arc::new(Mutex::new(Vec::new()));
    let sink_aborts = abort_jobs.clone();
    let on_event = Arc::new(move |event: &Value| {
        sink_store
            .append_run_event(&sink_run, event)
            .map_err(runtime_error)?;
        notify(&sink_runtime, || {
            sink_runtime
                .host
                .on_turn_event(&sink_session, &sink_channel, event)
        });
        let mut state = sink_state.lock().unwrap_or_else(|e| e.into_inner());
        state.events += 1;
        match event["type"].as_str() {
            Some("assistant_text_delta") => {
                state.text.push_str(event["text"].as_str().unwrap_or(""))
            }
            Some("permission_request" | "elicitation_request") => {
                state.fail(
                    "cron_interaction_required",
                    &format!(
                        "Cron run requested unsupported user interaction through {}.",
                        event["type"].as_str().unwrap()
                    ),
                    true,
                );
                if !state.abort_requested {
                    state.abort_requested = true;
                    let host = sink_runtime.host.clone();
                    let session = sink_session.clone();
                    let run = sink_run.clone();
                    // Waiting here would deadlock the native consumer-finally fence.
                    sink_aborts
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(tokio::spawn(async move {
                            host.abort(&session, &run, "system:interaction_required")
                                .await
                        }));
                }
            }
            Some("error") => {
                let code = event["code"].as_str().unwrap_or("cron_run_failed");
                if code == "turn_timeout" {
                    state.fail(
                        "cron_run_timeout",
                        event["message"].as_str().unwrap_or("Turn timed out."),
                        true,
                    );
                } else {
                    state.fail(
                        code,
                        event["message"].as_str().unwrap_or("Cron task failed."),
                        false,
                    );
                }
            }
            _ => {}
        }
        Ok(())
    });
    let request = NativeTurnRequest {
        project_key: store.project_key.clone(),
        session_key: session.clone(),
        run_id: run_id.clone(),
        channel_key: channel.clone(),
        message: string(&task, "message").to_owned(),
        cwd: PathBuf::from(&store.project_key),
        permission_mode: "bypassPermissions",
        can_prompt: false,
        excluded_tools: crate::config::session_overrides::UNATTENDED_SESSION_EXCLUDED_TOOLS
            .to_vec(),
        permission_rules: Value::Null,
        tools: None,
        timeout_ms: Some(
            runtime
                .tasks
                .config()
                .run_timeout_minutes
                .saturating_mul(60_000),
        ),
        on_event,
    };
    let mut cancelled = runtime
        .active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&run_id)
        .expect("claimed run is registered")
        .cancel
        .subscribe();
    let already_stopped = *cancelled.borrow();
    if !already_stopped {
        let execution = runtime.host.turn(request);
        tokio::pin!(execution);
        let result = tokio::select! {
            biased;
            result = &mut execution => result,
            _ = async { let _ = cancelled.wait_for(|stopped| *stopped).await; } => {
                match tokio::time::timeout(Duration::from_secs(5), &mut execution).await {
                    Ok(result) => result,
                    Err(_) => {
                        runtime.report(RuntimeError::new("cron_cancel_unsettled", "Native stopped turn did not settle."));
                        Err(RuntimeError::new("agent_aborted", "Cron task was stopped."))
                    }
                }
            },
        };
        let mut state = outcome.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Ok(events) if state.events == 0 || events.len() != state.events => {
                state.fail(
                    "cron_run_failed",
                    "Native host did not stream its actual turn events.",
                    false,
                );
            }
            Ok(_) => {}
            Err(error) => state.fail("cron_run_failed", &error.message, false),
        }
    }
    let aborts = std::mem::take(&mut *abort_jobs.lock().unwrap_or_else(|e| e.into_inner()));
    for mut abort in aborts {
        match tokio::time::timeout(Duration::from_secs(5), &mut abort).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => runtime.report(error),
            Ok(Err(error)) => {
                runtime.report(RuntimeError::new("cron_abort_failed", error.to_string()))
            }
            Err(_) => {
                abort.abort();
                runtime.report(RuntimeError::new(
                    "cron_abort_failed",
                    "Native abort did not settle.",
                ));
            }
        }
    }
    let finished_ms = runtime.tasks.now_ms();
    let finished = iso(finished_ms);
    let (result, error, text) = {
        let mut state = outcome.lock().unwrap_or_else(|e| e.into_inner());
        if runtime
            .remove_active(&run_id)
            .is_some_and(|run| run.stopped)
        {
            state.outcome = "stopped";
            if state.synthetic_abort_error {
                state.error = None;
            }
        }
        (
            state.outcome,
            state.error.clone(),
            state.text.trim().to_owned(),
        )
    };
    let mut terminal = json!({"schemaVersion":1,"runId":run_id,"taskId":task["taskId"],"sessionKey":session,
        "projectKey":store.project_key,"startedAt":started,"finishedAt":finished,"outcome":result});
    if let Some(error) = &error {
        terminal["error"] = error.clone();
    }
    if let Err(error) = store.append_run(&terminal).map_err(runtime_error) {
        runtime.report(error);
    }
    phase(
        runtime,
        &task,
        &run_id,
        if result == "completed" {
            "cron_completed"
        } else {
            "cron_failed"
        },
        &finished,
        &title,
        error.as_ref(),
    );
    let text = if result == "completed" {
        text
    } else {
        error
            .as_ref()
            .and_then(|e| e["message"].as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Cron task failed.")
            .trim()
            .to_owned()
    };
    let mut delivery = json!({"taskId":task["taskId"],"runId":run_id,"sessionKey":session,"channelKey":channel,
        "projectKey":store.project_key,"outcome":result,"text":if text.is_empty() { "定时任务已完成，但没有返回内容。" } else { &text }});
    for key in ["originSessionKey", "originChannelKey"] {
        if let Some(value) = task.get(key) {
            delivery[key] = value.clone();
        }
    }
    if let Some(error) = error {
        delivery["error"] = error;
    }
    if let Err(error) = runtime.host.deliver_result(&delivery).await {
        runtime.report(error);
    }
    let changed = store
        .finalize(
            &task,
            &run_id,
            finished_ms,
            &runtime.tasks.config().timezone,
        )
        .map_err(runtime_error)?;
    if changed && task["schedule"]["type"] == "once" {
        runtime.host.release_task(&task).await?;
    }
    Ok(())
}

fn notify(runtime: &CronRuntime, send: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(send)).is_err() {
        runtime.report(RuntimeError::new(
            "cron_event_delivery_failed",
            "Native notification callback panicked.",
        ));
    }
}
fn phase(
    runtime: &CronRuntime,
    task: &Value,
    run: &str,
    name: &str,
    timestamp: &str,
    title: &str,
    error: Option<&Value>,
) {
    let mut event = json!({"phase":name,"runId":run,"taskId":task["taskId"],"projectKey":task["projectKey"],"timestamp":timestamp,"title":title});
    if let Some(error) = error {
        event["error"] = error.clone();
    }
    notify(runtime, || runtime.host.on_phase_event(&event));
}
