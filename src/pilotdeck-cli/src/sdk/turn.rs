//! Shared run/resume control, once signals, and owned query lifecycle.
use super::{
    Connection,
    output::{EventError, TextOutput},
    positional, read_value, signals, transport,
};
use crate::{Failure, paths};
use futures_util::future::{Either, select};
use pilotdeck_protocol::{JsString, JsValue};
use serde::Serialize;
use serde_json::json;
use signals::Signals;
use std::io::{IsTerminal, Read};
use tokio::time::{Instant, timeout_at};
use transport::{QueryFrame, Transport, string_field};

pub(super) fn run(
    argv: &[String],
    connection: &Connection,
    resume: Option<&str>,
) -> Result<u8, Failure> {
    if read_value(argv, "--model").is_some() && read_value(argv, "--project").is_none() {
        // The TS query leaks its socket after this validation error. Keep the
        // exceptional lifecycle explicitly unimplemented until P06/P08.
        return Err(Failure::Todo(
            "pilotdeck run model without project (#123 P06/P08)",
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::Error(e.to_string()))?;
    runtime.block_on(run_connected(argv, connection, resume))
}

async fn run_connected(
    argv: &[String],
    connection: &Connection,
    resume: Option<&str>,
) -> Result<u8, Failure> {
    let project = read_value(argv, "--project");
    let mut control = Transport::connect(connection, "pilotdeck-cli", true).await?;
    let prepared = async {
        let session = if let Some(session) = resume {
            // runSdkCli keeps the argv ID, even if sessions.resume returns a
            // different key. The query resumes this same ID a second time.
            control
                .request("resume_session", json!({"sessionKey": session}))
                .await?;
            session.to_owned()
        } else {
            let created = control
                .request(
                    "new_session",
                    CreateSession {
                        project_key: project,
                        channel_key: read_value(argv, "--channel").unwrap_or("cli"),
                    },
                )
                .await?;
            string_field(&created, "sessionKey")
                .ok_or_else(|| Failure::Error("Gateway returned no sessionKey.".into()))?
        };
        let prompt = read_prompt(argv, usize::from(resume.is_some()))?;
        if prompt.is_empty() {
            return Err(Failure::Error(
                "A prompt is required, or provide it through stdin.".into(),
            ));
        }
        Ok((session, prompt))
    }
    .await;
    let (session, prompt) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            control.close().await;
            return Err(error);
        }
    };
    // TS installs both once listeners after the control RPC and prompt read,
    // before the separate query opens its socket or begins initialization.
    let mut signals = match Signals::install() {
        Ok(signals) => signals,
        Err(error) => {
            control.close().await;
            return Err(error);
        }
    };
    let mut query = None;
    let initialized = signals
        .during(async {
            query = Some(Transport::open(connection, true).await?);
            let query = query
                .as_mut()
                .expect("#1145 opened query retains its transport owner");
            query.hello(connection, "pilotdeck-cli").await?;
            initialize_turn(query, argv, &session, &prompt, project).await
        })
        .await;
    let outcome = match initialized {
        Ok(initialized) => {
            stream_turn(
                query
                    .as_mut()
                    .expect("#1145 initialized query retains its transport owner"),
                argv,
                &session,
                initialized,
                &mut signals,
            )
            .await
        }
        Err(error) if signals.count() == 0 && query.is_some() => QueryOutcome::Retain(error),
        Err(error) => QueryOutcome::InitializationFailed(error),
    };
    match outcome {
        QueryOutcome::Complete(output) => {
            if let Some(query) = &mut query {
                query.close().await;
            }
            let result = (*output).finish();
            signals.remove();
            control.close().await;
            result
        }
        QueryOutcome::Closed(error) => {
            if let Some(query) = &mut query {
                query.close().await;
            }
            signals.remove();
            control.close().await;
            Err(error)
        }
        QueryOutcome::InitializationFailed(error) => {
            // An abort awaiting the rejected initialization promise is an
            // unhandled rejection in TS. CLI finally closes only control;
            // query is subsequently dropped by process termination.
            signals.remove();
            control.close().await;
            if let Some(query) = &mut query {
                query.flush_pending().await;
            }
            Err(error)
        }
        QueryOutcome::Retain(error) => {
            // The TS iterator's error path does not close its query transport.
            // The CLI prints its error and closes control, but that query keeps
            // the process alive. Preserve the observed lifetime until peer
            // closure/default signal; do not add a new local exit deadline.
            signals.remove();
            control.close().await;
            match error {
                Failure::Error(message) => crate::stderr_line(&message),
                other => return Err(other),
            }
            if let Some(query) = &mut query {
                query.wait_peer_closed().await;
            }
            Ok(1)
        }
        QueryOutcome::RetainAborts {
            error,
            abort_ids,
            deadline,
        } => {
            // next() rejected, so CLI finally has completed. Its already
            // dispatched aborts remain pending on the separate query owner.
            signals.remove();
            control.close().await;
            match error {
                Failure::Error(message) => crate::stderr_line(&message),
                other => return Err(other),
            }
            if let Some(query) = &mut query {
                finish_pending_aborts(query, &abort_ids, deadline).await?;
            }
            Ok(1)
        }
        // A rejected in-flight abort terminates TS before CLI finally runs.
        // Raw owner drops deliberately omit WebSocket close frames on both
        // connections, unlike ordinary completion and initialization failure.
        QueryOutcome::FatalAbort(error) => Err(error),
    }
}

enum QueryOutcome {
    Complete(Box<TextOutput>),
    Closed(Failure),
    InitializationFailed(Failure),
    Retain(Failure),
    RetainAborts {
        error: Failure,
        abort_ids: Vec<String>,
        deadline: Instant,
    },
    FatalAbort(Failure),
}

struct InitializedTurn {
    request_id: String,
    run_id: String,
}

fn read_prompt(argv: &[String], offset: usize) -> Result<String, Failure> {
    let joined = positional(argv)[offset..].join(" ");
    let prompt = paths::js_trim(&joined);
    if !prompt.is_empty() {
        return Ok(prompt.to_owned());
    }
    if !std::io::stdin().is_terminal() {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|e| Failure::Error(e.to_string()))?;
        return Ok(paths::js_trim(&String::from_utf8_lossy(&bytes)).to_owned());
    }
    Ok(String::new())
}

async fn initialize_turn(
    query: &mut Transport,
    argv: &[String],
    session: &str,
    prompt: &str,
    project: Option<&str>,
) -> Result<InitializedTurn, Failure> {
    query
        .request("resume_session", json!({"sessionKey": session}))
        .await?;
    let model = match read_value(argv, "--model") {
        Some(model) => Some(resolve_model(query, project, model).await?),
        None => None,
    };
    let permission = read_value(argv, "--permission-mode");
    let sdk_permission = permission.filter(|p| matches!(*p, "acceptEdits" | "dontAsk"));
    let config = if let Some(permission) = sdk_permission {
        Some(json!({"permissionMode":permission}))
    } else if query
        .server_info
        .get("capabilities")
        .and_then(array)
        .is_some_and(|values| {
            values.iter().any(|v| {
                v.as_string()
                    .is_some_and(|v| v.equals_str("sdk_session_defaults"))
            })
        })
    {
        Some(json!({}))
    } else {
        None
    };
    let run_id = uuid::Uuid::new_v4().to_string();
    let request_id = query
        .start_stream(
            "submit_turn",
            SubmitTurn {
                session_key: session,
                channel_key: "api_server",
                project_key: project,
                message: prompt,
                can_prompt: false,
                can_elicit: false,
                mode: permission.map(|p| {
                    if matches!(p, "acceptEdits" | "dontAsk" | "auto") {
                        "default"
                    } else {
                        p
                    }
                }),
                base_permission_mode: permission.map(|p| {
                    if p == "bypassPermissions" {
                        p
                    } else {
                        "default"
                    }
                }),
                sdk_permission_mode: sdk_permission,
                sdk_session_config: config,
                model_override: model,
                run_id: run_id.clone(),
            },
        )
        .await?;
    Ok(InitializedTurn { request_id, run_id })
}

async fn stream_turn(
    query: &mut Transport,
    argv: &[String],
    session: &str,
    initialized: InitializedTurn,
    signals: &mut Signals,
) -> QueryOutcome {
    let stream_json = argv.iter().any(|arg| arg == "--stream-json");
    let mut output = TextOutput::new(
        session,
        &initialized.run_id,
        argv.iter().any(|arg| arg == "--json"),
        stream_json,
    );
    let mut abort_ids = Vec::with_capacity(2);
    let mut abort_deadline = None;
    let mut terminal_seen = false;
    loop {
        while abort_ids.len() < signals.count() {
            // Both signals can independently invoke abort while the first RPC
            // is still pending. Keep the submit stream's one socket reader.
            let deadline = Instant::now() + transport::REQUEST_TIMEOUT;
            let deadline = abort_deadline.map_or(deadline, |old: Instant| old.min(deadline));
            let sent = within_abort_deadline(
                deadline,
                query.begin_request(
                    "abort_turn",
                    AbortTurn {
                        session_key: session,
                        run_id: output.run_id(),
                        reason: "CLI interrupted",
                    },
                ),
            )
            .await;
            match sent {
                Ok(id) => abort_ids.push(id),
                Err(error) => return QueryOutcome::FatalAbort(error),
            }
            abort_deadline = Some(deadline);
        }
        let next = next_query_frame(query, &initialized.request_id, &abort_ids, signals);
        let incoming = if let Some(deadline) = abort_deadline {
            match timeout_at(deadline, next).await {
                Ok(incoming) => incoming,
                Err(_) => {
                    return QueryOutcome::FatalAbort(Failure::Error(
                        "Gateway request timed out: abort_turn".into(),
                    ));
                }
            }
        } else {
            next.await
        };
        match incoming {
            QueryInput::Signal => {}
            QueryInput::Frame(Err(error)) => {
                return if abort_ids.is_empty() {
                    QueryOutcome::Retain(error)
                } else {
                    QueryOutcome::InitializationFailed(error)
                };
            }
            QueryInput::Frame(Ok(QueryFrame::StreamFailed(error))) => {
                return retain_query(error, abort_ids, abort_deadline);
            }
            QueryInput::Frame(Ok(QueryFrame::AbortReply(Err(error)))) => {
                return QueryOutcome::FatalAbort(error);
            }
            QueryInput::Frame(Ok(QueryFrame::AbortReply(Ok(_)))) => {
                return QueryOutcome::Closed(client_closed());
            }
            QueryInput::Frame(Ok(QueryFrame::Event(event, final_frame))) => {
                if let Err(error) = output.event(&event) {
                    return match error {
                        EventError::Iterator(error) => {
                            retain_query(error, abort_ids, abort_deadline)
                        }
                        EventError::Consumer(error) => QueryOutcome::Closed(error),
                    };
                }
                if stream_json
                    && !terminal_seen
                    && matches!(
                        string_field(&event, "type").as_deref(),
                        Some("turn_completed" | "error")
                    )
                {
                    terminal_seen = true;
                    signals::observe("terminal-consumed");
                }
                if final_frame {
                    return if abort_ids.is_empty() {
                        QueryOutcome::Complete(Box::new(output))
                    } else {
                        // Closing a naturally completed stream rejects the
                        // still-pending abort; it must not become exit zero.
                        QueryOutcome::Closed(client_closed())
                    };
                }
            }
            QueryInput::Frame(Ok(QueryFrame::StreamDone)) => {
                return if abort_ids.is_empty() {
                    QueryOutcome::Complete(Box::new(output))
                } else {
                    QueryOutcome::Closed(client_closed())
                };
            }
        }
    }
}

async fn within_abort_deadline<T>(
    deadline: Instant,
    request: impl std::future::Future<Output = Result<T, Failure>>,
) -> Result<T, Failure> {
    timeout_at(deadline, request)
        .await
        .map_err(|_| Failure::Error("Gateway request timed out: abort_turn".into()))?
}

fn retain_query(error: Failure, abort_ids: Vec<String>, deadline: Option<Instant>) -> QueryOutcome {
    match deadline {
        Some(deadline) => QueryOutcome::RetainAborts {
            error,
            abort_ids,
            deadline,
        },
        None => QueryOutcome::Retain(error),
    }
}

async fn finish_pending_aborts(
    query: &mut Transport,
    abort_ids: &[String],
    deadline: Instant,
) -> Result<(), Failure> {
    loop {
        match timeout_at(deadline, query.query_frame(None, abort_ids)).await {
            Ok(Ok(QueryFrame::AbortReply(result))) => {
                result?;
                query.close().await;
                // The first successful abort closes the transport. Any second
                // outstanding abort rejects with Gateway client closed.
                return if abort_ids.len() == 1 {
                    Ok(())
                } else {
                    Err(client_closed())
                };
            }
            Ok(Err(error)) => {
                query.flush_pending().await;
                return Err(error);
            }
            Err(_) => {
                return Err(Failure::Error(
                    "Gateway request timed out: abort_turn".into(),
                ));
            }
            // With no stream id only abort responses are returned. Keep this
            // exhaustive if another non-stream notification is added later.
            Ok(Ok(_)) => {}
        }
    }
}

enum QueryInput {
    Signal,
    Frame(Result<QueryFrame, Failure>),
}

async fn next_query_frame(
    query: &mut Transport,
    request_id: &str,
    abort_ids: &[String],
    signals: &mut Signals,
) -> QueryInput {
    match select(
        Box::pin(query.query_frame(Some(request_id), abort_ids)),
        Box::pin(signals.next()),
    )
    .await
    {
        Either::Left((frame, _)) => QueryInput::Frame(frame),
        Either::Right(((), _)) => QueryInput::Signal,
    }
}

fn client_closed() -> Failure {
    Failure::Error("Gateway client closed.".into())
}

async fn resolve_model(
    query: &mut Transport,
    project: Option<&str>,
    model: &str,
) -> Result<ModelOverride, Failure> {
    let catalog = query
        .request("model_catalog_list", json!({"projectKey":project}))
        .await?;
    select_model(&catalog, model)
}

fn select_model(catalog: &JsValue, model: &str) -> Result<ModelOverride, Failure> {
    let candidates = catalog.get("items").and_then(array).unwrap_or(&[]);
    let matches: Vec<_> = candidates
        .iter()
        .filter(|item| {
            item.get("available").and_then(JsValue::as_bool) != Some(false)
                && (string_field(item, "id").as_deref() == Some(model)
                    || string_field(item, "model").as_deref() == Some(model)
                    || format!(
                        "{}/{}",
                        string_field(item, "provider").unwrap_or_else(|| "undefined".into()),
                        string_field(item, "model").unwrap_or_else(|| "undefined".into())
                    ) == model)
        })
        .collect();
    if let [item] = matches.as_slice()
        && let (Some(provider), Some(model)) =
            (string_field(item, "provider"), string_field(item, "model"))
        && !provider.is_empty()
        && !model.is_empty()
    {
        return Ok(ModelOverride {
            mode: "model",
            provider,
            model,
        });
    }
    Err(Failure::Error(format!(
        "Model is not uniquely resolvable: {model}"
    )))
}

// Field order follows the observed SDK JSON.stringify request, including omitted
// optional keys. A serde_json::Value would sort these object keys.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSession<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    project_key: Option<&'a str>,
    channel_key: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AbortTurn<'a> {
    session_key: &'a str,
    #[serde(serialize_with = "serialize_run_id")]
    run_id: &'a JsString,
    reason: &'static str,
}

fn serialize_run_id<S: serde::Serializer>(
    run_id: &JsString,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // serde_json strings require Unicode scalar values. RawValue preserves
    // JSON.stringify's escaped lone UTF-16 surrogates without replacement.
    let encoded = pilotdeck_protocol::stringify_json(&JsValue::String(run_id.clone()))
        .map_err(serde::ser::Error::custom)?
        .ok_or_else(|| serde::ser::Error::custom("#1145 runId string must serialize"))?;
    serde_json::value::RawValue::from_string(encoded)
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmitTurn<'a> {
    session_key: &'a str,
    channel_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_key: Option<&'a str>,
    message: &'a str,
    can_prompt: bool,
    can_elicit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_permission_mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sdk_permission_mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sdk_session_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_override: Option<ModelOverride>,
    run_id: String,
}
#[derive(Serialize)]
struct ModelOverride {
    mode: &'static str,
    provider: String,
    model: String,
}

fn array(value: &JsValue) -> Option<&[JsValue]> {
    match value {
        JsValue::Array(values) => Some(values),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_negative_receipt::verify;

    #[tokio::test(flavor = "current_thread")]
    async fn abort_send_respects_predecessor_deadline() {
        let past_deadline = Instant::now();
        let blocked_send = std::future::pending::<Result<(), Failure>>();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            within_abort_deadline(past_deadline, blocked_send),
        )
        .await
        .expect("#1145 blocked second abort cannot extend first deadline");
        assert!(
            matches!(result, Err(Failure::Error(message)) if message == "Gateway request timed out: abort_turn"),
            "#1145 blocked abort reports original deadline"
        );
    }

    #[test]
    fn abort_uses_consumed_run_id_without_utf16_loss() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/sdk-signal-abort-state/oracle.json"
        ))
        .expect("#1145 supplemental runId oracle JSON");
        let rows: Vec<_> = oracle["cases"]
            .as_array()
            .expect("#1145 supplemental runId oracle rows")
            .iter()
            .filter(|row| row["kind"] == "run-id")
            .collect();
        assert_eq!(rows.len(), 6, "#1145 fixed runId state matrix");
        for row in rows {
            let mut output = TextOutput::new("s", "requested-run", true, false);
            let event = pilotdeck_protocol::parse_json(
                row["input"]["eventJson"]
                    .as_str()
                    .expect("#1145 runId encoded event input"),
            )
            .expect("#1145 runId event JSON");
            output.event(&event).expect("#1145 consume updated runId");
            let encoded_abort = serde_json::to_string(&AbortTurn {
                session_key: "s",
                run_id: output.run_id(),
                reason: "CLI interrupted",
            })
            .expect("#1145 encode updated abort runId");
            assert_eq!(
                encoded_abort,
                row["output"]["abortParamsJson"]
                    .as_str()
                    .expect("#1145 TS encoded abort params"),
                "#1145 abort uses latest lossless runId"
            );
        }
    }

    #[test]
    fn output_failure_preserves_iterator_vs_consumer_lifetime() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/sdk-signal-abort-state/oracle.json"
        ))
        .expect("#1145 supplemental error oracle JSON");
        let rows: Vec<_> = oracle["cases"]
            .as_array()
            .expect("#1145 supplemental error oracle rows")
            .iter()
            .filter(|row| row["kind"] == "event-error")
            .collect();
        assert_eq!(rows.len(), 3, "#1145 fixed event error state matrix");
        for row in rows {
            let mut output = TextOutput::new("s", "r", true, false);
            let event = pilotdeck_protocol::parse_json(
                row["input"]["eventJson"]
                    .as_str()
                    .expect("#1145 error encoded event input"),
            )
            .expect("#1145 lifetime error event JSON");
            let (kind, failure) = match output.event(&event) {
                Err(EventError::Consumer(failure)) => ("consumer", failure),
                Err(EventError::Iterator(failure)) => ("iterator", failure),
                Ok(()) => panic!("#1145 TS event error must reject"),
            };
            let Failure::Error(message) = failure else {
                panic!("#1145 event error has concrete message");
            };
            assert_eq!(
                kind,
                row["output"]["errorKind"]
                    .as_str()
                    .expect("#1145 TS error lifetime classification"),
                "#1145 event error matches TS iterator or consumer lifetime"
            );
            assert_eq!(
                message,
                row["output"]["message"]
                    .as_str()
                    .expect("#1145 TS event error message"),
                "#1145 event error preserves TS message"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "#1145 owns mock 28999; run explicitly without another replay"]
    async fn pending_abort_keeps_query_until_reply() {
        use futures_util::{SinkExt, StreamExt};
        use tokio::{
            net::TcpListener,
            time::{Duration, timeout},
        };
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/sdk-signal-abort-state/oracle.json"
        ))
        .expect("#1145 pending-abort TS state oracle");
        let rows: Vec<_> = oracle["cases"]
            .as_array()
            .expect("#1145 pending-abort TS rows")
            .iter()
            .filter(|row| row["kind"] == "pending-abort")
            .collect();
        assert_eq!(rows.len(), 2, "#1145 fixed pending-abort state matrix");
        for row in rows {
            let sequence_gap = row["input"]["failure"] == "sequence-gap";
            timeout(Duration::from_secs(3), async {
                let listener = TcpListener::bind("127.0.0.1:28999").await
                    .expect("#1145 pending-abort owned mock port");
                let connection = Connection {
                    gateway_url: "ws://127.0.0.1:28999/ws".into(), auth_token: "fixture".into(),
                };
                let accept = async {
                    let (socket, _) = listener.accept().await.expect("#1145 pending-abort accept");
                    accept_async(socket).await.expect("#1145 pending-abort handshake")
                };
                let (client, mut peer) = tokio::join!(Transport::open(&connection, true), accept);
                let mut client = client.expect("#1145 pending-abort connected owner");
                let abort = client.begin_request("abort_turn", json!({"sessionKey":"s"})).await
                    .expect("#1145 pending-abort request sent");
                peer.next().await.expect("#1145 pending-abort request exists")
                    .expect("#1145 pending-abort valid request");
                let failed = if sequence_gap {
                    json!({"type":"event","id":"stream","seq":2,"event":{"type":"turn_started"}})
                } else {
                    json!({"type":"response","id":"stream","ok":false,"error":{"message":"stream rejected"}})
                };
                peer.send(Message::Text(failed.to_string().into())).await
                    .expect("#1145 send stream-only failure");
                let abort_ids = vec![abort.clone()];
                let frame = client.query_frame(Some("stream"), &abort_ids).await
                    .expect("#1145 stream failure preserves transport");
                let QueryFrame::StreamFailed(error) = frame else {
                    panic!("#1145 only failed stream is retired");
                };
                let deadline = Instant::now() + Duration::from_secs(1);
                let QueryOutcome::RetainAborts { abort_ids, deadline, .. } =
                    retain_query(error, abort_ids, Some(deadline)) else {
                    panic!("#1145 failed stream retains pending abort owner");
                };
                assert_eq!(json!(abort_ids.len()), row["output"]["beforeReply"]["pendingAbort"],
                    "#1145 retained abort count matches TS");
                let mut pending = Box::pin(finish_pending_aborts(&mut client, &abort_ids, deadline));
                let waiting = std::future::poll_fn(|cx| std::task::Poll::Ready(
                    std::future::Future::poll(pending.as_mut(), cx).is_pending()
                )).await;
                assert!(waiting,
                    "#1145 abort cannot finish before its reply");
                // A stale stream completion cannot settle the surviving abort.
                peer.send(Message::Text(json!({"type":"response","id":"stream","ok":true}).to_string().into()))
                    .await.expect("#1145 late stream frame");
                peer.send(Message::Text(json!({"type":"response","id":abort,"ok":true,"result":{"aborted":true}}).to_string().into()))
                    .await.expect("#1145 abort reply");
                pending.await.expect("#1145 replied abort completes");
                assert!(matches!(peer.next().await, Some(Ok(Message::Close(_)))),
                    "#1145 replied abort releases query connection");
                let _ = peer.flush().await;
            }).await.expect("#1145 pending-abort test bounded cleanup");
        }
    }

    #[test]
    fn model_resolution_matches_ts_boundaries() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/sdk-run-validation/oracle.json"
        ))
        .expect("#1058 model validation oracle");
        let rows = oracle["models"].as_array().expect("#1058 model rows");
        verify(rows.len() == 12, "#1058 registered model validation rows");
        for case in rows {
            let input = &case["input"];
            let catalog =
                pilotdeck_protocol::parse_json(&json!({"items":input["items"]}).to_string())
                    .expect("#1058 model catalog JSON");
            let result = select_model(
                &catalog,
                input["model"].as_str().expect("#1058 requested model"),
            );
            let actual = match result {
                Ok(value) => json!({"value":value}),
                Err(Failure::Error(message)) => json!({"error":message}),
                Err(other) => json!({"unexpected":format!("{other:?}")}),
            };
            if actual != case["output"] {
                eprintln!(
                    "#1058 model {}: {actual}, expected {}",
                    input["name"], case["output"]
                );
            }
            verify(actual == case["output"], "#1058 model resolution bytes");
        }
    }
}
