//! Native WS transport. The historical upstream pump is compiled only for fixtures.
#[cfg(test)]
use crate::runtime::connection::RequestContext;
#[cfg(test)]
use crate::upstream;
use crate::{
    acceptor::{self, Action},
    dispatch::{self, Route},
    frame::{self, Decoder},
    runtime::interaction::GatewayInteractionCoordinator,
    runtime::mcp::routes::McpRoutes,
};
use std::{collections::VecDeque, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Notify;
#[cfg(test)]
use tokio::{sync::Mutex, task::JoinSet};

#[cfg(test)]
struct Downstream<W> {
    writer: W,
    closed: bool,
    authenticated: bool,
    handshake_started: bool,
    invalidated: bool,
    pings_pending: usize,
    context: RequestContext,
    binding: Option<pilotdeck_protocol::JsValue>,
}
#[cfg(test)]
struct PairedServices {
    interactions: Arc<GatewayInteractionCoordinator>,
    dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
    mcp: Option<Arc<McpRoutes>>,
    services: Arc<crate::routes::GatewayServices>,
    automation: Option<Arc<crate::cron::Automation>>,
}
#[cfg(test)]
impl Default for PairedServices {
    fn default() -> Self {
        Self {
            interactions: Arc::default(),
            dialogs: Arc::default(),
            mcp: None,
            services: Arc::new(crate::routes::GatewayServices::from_env()),
            automation: None,
        }
    }
}

#[cfg(test)]
pub(crate) async fn run<D, U>(
    downstream: D,
    upstream: U,
    mcp: Option<Arc<McpRoutes>>,
) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    run_with_domain_state(
        downstream,
        upstream,
        None::<crate::metadata::Metadata>,
        PairedServices {
            mcp,
            ..Default::default()
        },
    )
    .await
}

#[cfg(test)]
async fn run_with_routes<D, U, F>(
    downstream: D,
    upstream: U,
    metadata: Option<crate::metadata::Metadata<F>>,
    mcp: Option<Arc<McpRoutes>>,
) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
    F: crate::metadata::Files + Send + Sync + 'static,
{
    run_with_domain_state(
        downstream,
        upstream,
        metadata,
        PairedServices {
            mcp,
            ..Default::default()
        },
    )
    .await
}

#[cfg(test)]
async fn run_with_metadata<D, U, F>(
    downstream: D,
    upstream: U,
    metadata: Option<crate::metadata::Metadata<F>>,
) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
    F: crate::metadata::Files + Send + Sync + 'static,
{
    run_with_state(downstream, upstream, metadata, Arc::default()).await
}

#[cfg(test)]
async fn run_with_state<D, U, F>(
    downstream: D,
    upstream: U,
    metadata: Option<crate::metadata::Metadata<F>>,
    interactions: Arc<GatewayInteractionCoordinator>,
) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
    F: crate::metadata::Files + Send + Sync + 'static,
{
    run_with_domain_state(
        downstream,
        upstream,
        metadata,
        PairedServices {
            interactions,
            ..Default::default()
        },
    )
    .await
}

#[cfg(test)]
async fn run_with_domain_state<D, U, F>(
    downstream: D,
    upstream: U,
    metadata: Option<crate::metadata::Metadata<F>>,
    domains: PairedServices,
) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
    F: crate::metadata::Files + Send + Sync + 'static,
{
    let PairedServices {
        interactions,
        dialogs,
        mcp,
        services,
        automation,
    } = domains;
    let _host_scope = crate::config::routes::native::paired_scope(metadata.as_ref());
    let upstream_turns =
        crate::config::routes::upstream_turns::UpstreamTurns::new(metadata.as_ref());
    let activity = automation.as_ref().map(|a| a.activity.guard());
    let metadata = metadata.map(Arc::new);
    let mut config_notices = crate::config::routes::native::subscribe();
    let (down_read, down_write) = tokio::io::split(downstream);
    let (mut up_read, up_write) = tokio::io::split(upstream);
    let down_write = Mutex::new(Downstream {
        writer: down_write,
        closed: false,
        authenticated: false,
        handshake_started: false,
        invalidated: false,
        pings_pending: 0,
        context: RequestContext::default(),
        binding: None,
    });
    let ping_completed = Notify::new();
    let receive = async {
        let mut decoder = Decoder::default();
        loop {
            match decoder.next_server() {
                Ok(Some(frame)) => {
                    if let Some(activity) = &activity
                        && frame.opcode == 1
                        && let Some(parsed) = dispatch::parse(&frame.payload)
                        && let Ok(value) = crate::cron::native::from_js(&parsed)
                    {
                        activity.activity.observe(&activity.writer, &value);
                    }
                    let mut downstream = down_write.lock().await;
                    if frame.opcode == 8 {
                        // Future local handlers must never synthesize output
                        // after TS closes, even before the upstream EOF arrives.
                        downstream.closed = true;
                    }
                    if frame.opcode == 1
                        && let Some(value) = dispatch::parse(&frame.payload)
                    {
                        upstream_turns.response(&value);
                    }
                    downstream.writer.write_all(&frame.wire).await?;
                    if frame.opcode == 10 {
                        downstream.pings_pending = downstream.pings_pending.saturating_sub(1);
                        ping_completed.notify_one();
                    }
                    if frame.opcode == 1
                        && !downstream.invalidated
                        && !downstream.closed
                        && let Some(hello) = dispatch::parse(&frame.payload)
                        && crate::metadata::text_field(&hello, "type").as_deref()
                            == Some("hello_ok")
                        && crate::metadata::text_field(&hello, "protocolVersion").as_deref()
                            == Some(pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION)
                        && hello
                            .get("interactionBinding")
                            .and_then(pilotdeck_protocol::JsValue::as_object)
                            .is_some()
                    {
                        downstream.authenticated = true;
                        downstream.binding = hello.get("interactionBinding").cloned();
                    }
                }
                Ok(None) => {
                    if up_read.read_buf(&mut decoder.buffer).await? == 0 {
                        // Preserve a truncated upstream tail too, then close.
                        down_write
                            .lock()
                            .await
                            .writer
                            .write_all(&decoder.buffer)
                            .await?;
                        return Ok(());
                    }
                }
                Err(_) => {
                    // Upstream owns its wire even for malformed output. Keep
                    // the lock during fallback so locally generated replies
                    // cannot corrupt an unparseable frame in flight.
                    let mut downstream = down_write.lock().await;
                    downstream.writer.write_all(&decoder.buffer).await?;
                    tokio::io::copy(&mut up_read, &mut downstream.writer).await?;
                    return Ok(());
                }
            }
        }
    };
    tokio::select! {
        result = send(down_read, up_write, &down_write, metadata.as_ref(), &ping_completed, &upstream_turns, PairedSend {mcp: mcp.as_ref(), interactions: &interactions, dialogs: &dialogs, services: &services, automation, activity: activity.as_ref()}) => result,
        result = receive => result,
        result = async {
            loop {
                let notice = match config_notices.recv().await {
                    Ok(notice) => notice,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                };
                let (home, cwd, notice) = notice;
                if metadata.as_ref().is_none_or(|m| m.home != home || m.cwd != cwd) { continue; }
                let mut state = down_write.lock().await;
                if !state.authenticated || state.invalidated || state.closed { continue; }
                let text = pilotdeck_protocol::stringify_json(&notice).expect("notification").expect("object");
                state.writer.write_all(&frame::server_frame(1, text.as_bytes())).await?;
            }
        } => result,
    }
}

#[cfg(test)]
struct PairedSend<'a> {
    mcp: Option<&'a Arc<McpRoutes>>,
    interactions: &'a Arc<GatewayInteractionCoordinator>,
    dialogs: &'a Arc<crate::user_dialog::GatewayUserDialogs>,
    services: &'a Arc<crate::routes::GatewayServices>,
    automation: Option<Arc<crate::cron::Automation>>,
    activity: Option<&'a crate::cron::activity::ConnectionActivity>,
}
#[cfg(test)]
async fn send<R, U, D, F>(
    mut reader: R,
    mut writer: U,
    downstream: &Mutex<Downstream<D>>,
    metadata: Option<&Arc<crate::metadata::Metadata<F>>>,
    ping_completed: &Notify,
    upstream_turns: &crate::config::routes::upstream_turns::UpstreamTurns,
    domains: PairedSend<'_>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    U: AsyncWrite + Unpin,
    D: AsyncWrite + Unpin,
    F: crate::metadata::Files + Send + Sync + 'static,
{
    let PairedSend {
        mcp,
        interactions,
        dialogs,
        services,
        automation,
        activity,
    } = domains;
    let mut decoder = Decoder::default();
    let mut jobs = JoinSet::new();
    let mut ready = VecDeque::new();
    let mut notifications = automation.as_ref().map(|a| a.subscribe());
    loop {
        match decoder.next() {
            Ok(Some(frame)) => match acceptor::action(&frame) {
                Action::Text => {
                    let request = dispatch::parse(&frame.payload);
                    let (eligible, interaction_params) = {
                        let mut state = downstream.lock().await;
                        if !state.handshake_started {
                            state.handshake_started = true;
                            if !request.as_ref().is_some_and(dispatch::is_hello) {
                                state.invalidated = true;
                            } else {
                                state.context.client_name = request
                                    .as_ref()
                                    .and_then(|hello| hello.get("clientName"))
                                    .and_then(pilotdeck_protocol::JsValue::as_string)
                                    .cloned();
                            }
                        } else if !request.as_ref().is_some_and(dispatch::is_request) {
                            // This can be queued while hello_ok is in flight.
                            // TS is already authed when it sends hello_ok, so
                            // invalid JSON / nonrequests will close the pair.
                            state.invalidated = true;
                        }
                        let eligible = state.authenticated && !state.invalidated && !state.closed;
                        let interaction_params =
                            if eligible && request.as_ref().is_some_and(dispatch::is_request) {
                                let request = request.as_ref().expect("authenticated request");
                                let method = request
                                    .get("method")
                                    .and_then(pilotdeck_protocol::JsValue::as_string)
                                    .expect("validated request");
                                let binding = state.binding.clone().expect("authenticated binding");
                                if [
                                    "permission_decide",
                                    "elicitation_respond",
                                    "submit_turn",
                                    "reconnect_interaction",
                                ]
                                .iter()
                                .any(|name| method.equals_str(name))
                                {
                                    let params = state.context.inject(
                                        method,
                                        request
                                            .get("params")
                                            .cloned()
                                            .unwrap_or(pilotdeck_protocol::JsValue::Undefined),
                                        &binding,
                                    );
                                    ["permission_decide", "elicitation_respond"]
                                        .iter()
                                        .any(|name| method.equals_str(name))
                                        .then_some(params)
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                        (eligible, interaction_params)
                    };
                    if eligible && request.as_ref().is_some_and(is_automation) {
                        let request = request.expect("validated automation request");
                        let automation = automation.clone();
                        jobs.spawn(async move {
                            (
                                frame,
                                vec![Route::LocalResponse(
                                    automation_response(automation, &request).await,
                                )],
                            )
                        });
                    } else if eligible
                        && matches!(
                            request
                                .as_ref()
                                .and_then(|request| crate::metadata::text_field(request, "method"))
                                .as_deref(),
                            Some(
                                "user_dialog_list"
                                    | "user_dialog_claim"
                                    | "user_dialog_release"
                                    | "user_dialog_respond"
                            )
                        )
                    {
                        let request = request.expect("validated dialog request");
                        let dialogs = dialogs.clone();
                        jobs.spawn(async move {
                            let route = crate::routes::user_dialog(&request, &dialogs)
                                .await
                                .expect("owned dialog method");
                            (frame, vec![route])
                        });
                    } else if let Some(params) = interaction_params {
                        let routes = crate::routes::bound_interaction(
                            request.as_ref().unwrap(),
                            &params,
                            interactions,
                        );
                        ready.push_back((frame, routes));
                    } else if eligible
                        && request.as_ref().is_some_and(crate::routes::native_request)
                    {
                        let request = request.expect("native request");
                        let services = services.clone();
                        jobs.spawn(async move {
                            let route = crate::routes::native_route(&services, &request).await;
                            (frame, vec![route])
                        });
                    } else if eligible
                        && request.as_ref().is_some_and(dispatch::is_local)
                        && !(mcp.is_none()
                            && metadata.is_none()
                            && request.as_ref().is_some_and(crate::routes::is_mcp))
                    {
                        let request = request.expect("eligible local request");
                        if crate::routes::is_mcp(&request) {
                            let mcp = mcp.cloned();
                            jobs.spawn(async move {
                                let route =
                                    crate::routes::route_mcp(&request, mcp.as_deref()).await;
                                (frame, vec![route])
                            });
                        } else if dispatch::needs_files(&request) {
                            let metadata = metadata.cloned();
                            let interactions = interactions.clone();
                            // Completion is polled only after the current input
                            // buffer: TS dispatches promises without blocking
                            // its frame parser, so coalesced close/ping wins.
                            jobs.spawn_blocking(move || {
                                let route = crate::routes::route_with_interactions(
                                    &request,
                                    metadata.as_deref(),
                                    &interactions,
                                );
                                (frame, vec![route])
                            });
                        } else {
                            match crate::routes::route_with_interactions(
                                &request,
                                metadata.map(Arc::as_ref),
                                interactions,
                            ) {
                                Route::Forward => forward_or_drain(&mut writer, &frame).await,
                                route => ready.push_back((frame, vec![route])),
                            }
                        }
                    } else {
                        if eligible && let Some(request) = request.as_ref() {
                            upstream_turns.request(request);
                            if let Some(activity) = activity
                                && crate::metadata::text_field(request, "method").as_deref()
                                    == Some("submit_turn")
                                && let Some(params) = request.get("params")
                                && let Ok(params) = crate::cron::native::from_js(params)
                            {
                                let run = params["runId"].as_str().unwrap_or("upstream");
                                activity.activity.begin(&activity.writer, run, &params);
                            }
                        }
                        forward_or_drain(&mut writer, &frame).await;
                    }
                }
                Action::Ping => {
                    downstream.lock().await.pings_pending += 1;
                    forward_or_drain(&mut writer, &frame).await;
                }
                Action::Close => {
                    downstream.lock().await.closed = true;
                    forward_or_drain(&mut writer, &frame).await;
                    // TS returns from handleData on close, ignoring subsequent
                    // coalesced frames. Keep both streams alive until receive
                    // drains TS's queued event/response/close and reaches EOF.
                    return std::future::pending().await;
                }
                Action::Ignore => {}
            },
            Ok(None) => {
                let can_emit = downstream.lock().await.pings_pending == 0;
                if can_emit && let Some((frame, routes)) = ready.pop_front() {
                    for route in routes {
                        emit_or_forward(&mut writer, downstream, &frame, route).await?;
                    }
                    continue;
                }
                tokio::select! {
                    biased;
                    result = jobs.join_next(), if can_emit && !jobs.is_empty() => {
                        let (frame, routes) = result.expect("nonempty jobs").map_err(std::io::Error::other)?;
                        for route in routes { emit_or_forward(&mut writer, downstream, &frame, route).await?; }
                    }
                    result = reader.read_buf(&mut decoder.buffer) => {
                        if result? == 0 { return Ok(()); }
                    }
                    _ = ping_completed.notified(), if !can_emit => {},
                    event = receive_notification(&mut notifications), if notifications.is_some() && can_emit => {
                        if let Some(event)=event {
                            let mut state=downstream.lock().await;
                            if state.authenticated && !state.closed && !state.invalidated {
                                state.writer.write_all(&frame::server_frame(1,&notification_bytes(event))).await?;
                            }
                        }
                    },
                }
            }
            Err(_) => {
                // TS's parser throws on unmasked/unsafe lengths. Its CLI closes
                // the listener but can leave this socket open during shutdown.
                // Preserve that observable exception path (A2 /health oracle),
                // with no synthesized WS close and no upstream validation.
                writer.write_all(&decoder.buffer).await?;
                tokio::io::copy(&mut reader, &mut writer).await?;
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
async fn emit_or_forward<U: AsyncWrite + Unpin, D: AsyncWrite + Unpin>(
    upstream: &mut U,
    downstream: &Mutex<Downstream<D>>,
    frame: &frame::Frame,
    route: Route,
) -> std::io::Result<()> {
    match route {
        Route::Forward => forward_or_drain(upstream, frame).await,
        Route::LocalResponse(payload) => {
            let mut state = downstream.lock().await;
            if !state.closed && !state.invalidated {
                state
                    .writer
                    .write_all(&frame::server_frame(1, &payload))
                    .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
async fn forward_or_drain<W: AsyncWrite + Unpin>(writer: &mut W, frame: &frame::Frame) {
    if upstream::forward(writer, frame).await.is_err() {
        // TS may have ended its socket after an earlier text while its close
        // is still buffered in the read direction. A failed write must not
        // cancel receive and discard that output; receive owns final teardown.
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests;

/// Local transport: runtime owns hello validation/binding; metadata keeps its
/// original request (including absent params) instead of normalizing JS values.
#[cfg(test)]
pub(crate) async fn run_local<S>(
    stream: S,
    token: String,
    mcp: Option<Arc<McpRoutes>>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    run_local_with_metadata(stream, token, crate::metadata::Metadata::from_env(), mcp).await
}

#[cfg(test)]
pub(crate) async fn run_local_with_metadata<S>(
    stream: S,
    token: String,
    metadata: Option<crate::metadata::Metadata>,
    mcp: Option<Arc<McpRoutes>>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    run_local_with_state(
        stream,
        token,
        LocalState {
            metadata,
            mcp,
            ..LocalState::default()
        },
    )
    .await
}

pub(crate) struct LocalState {
    pub metadata: Option<crate::metadata::Metadata>,
    pub interactions: Arc<GatewayInteractionCoordinator>,
    pub dialogs: Arc<crate::user_dialog::GatewayUserDialogs>,
    pub mcp: Option<Arc<McpRoutes>>,
    pub services: Arc<crate::routes::GatewayServices>,
    pub automation: Option<Arc<crate::cron::Automation>>,
    pub fallback: Option<crate::runtime::session::routes::passthrough::Upgrade>,
}
impl Default for LocalState {
    fn default() -> Self {
        Self {
            metadata: crate::metadata::Metadata::from_env().ok().flatten(),
            interactions: Arc::default(),
            dialogs: Arc::default(),
            mcp: None,
            services: Arc::new(crate::routes::GatewayServices::from_env()),
            automation: None,
            fallback: None,
        }
    }
}

pub(crate) async fn run_local_with_state<S>(
    stream: S,
    token: String,
    state: LocalState,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let gateway = Arc::new(LocalGateway {
        interactions: state.interactions.clone(),
        services: state.services.clone(),
    });
    run_local_with_bindings(stream, token, state, gateway).await
}

pub(crate) async fn run_local_with_gateway<S>(
    stream: S,
    token: String,
    gateway: Arc<dyn crate::runtime::connection::ConnectionGateway>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    run_local_with_bindings(stream, token, LocalState::default(), gateway).await
}

async fn run_local_with_bindings<S>(
    stream: S,
    token: String,
    state: LocalState,
    gateway: Arc<dyn crate::runtime::connection::ConnectionGateway>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use crate::runtime::connection::{
        Call, GatewayConnection, GatewayTextConnection, RequestError,
    };
    use std::{collections::VecDeque, future::poll_fn, task::Poll};

    let LocalState {
        metadata,
        interactions,
        dialogs,
        mcp,
        services,
        automation,
        fallback,
    } = state;
    let _host_scope = crate::config::routes::native::paired_scope(metadata.as_ref());
    let upstream_turns = Arc::new(crate::config::routes::upstream_turns::UpstreamTurns::new(
        metadata.as_ref(),
    ));
    let (forwarded, mut upstream_output) = tokio::sync::mpsc::unbounded_channel();
    let fallback = fallback.map(|upgrade| {
        crate::runtime::session::routes::passthrough::Passthrough::with_turns(
            upgrade,
            forwarded,
            Some(upstream_turns.clone()),
        )
    });
    let mut hello = Vec::new();
    let metadata = metadata.map(Arc::new);
    let mut config_notices = crate::config::routes::native::subscribe();
    let mut wrapper_capabilities = vec!["user_dialog_list".to_owned()];
    if let Some(automation) = &automation {
        if automation.has_cron_provider() {
            wrapper_capabilities.extend(
                crate::cron::METHODS
                    .iter()
                    .filter(|name| name.starts_with("cron_"))
                    .map(|name| (*name).to_owned()),
            );
        }
        wrapper_capabilities.extend(
            [
                "always_on_apply",
                "always_on_abort",
                "always_on_rerun_plan",
                "background_tasks",
                "background_task_stop",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    // These services are dispatched by this transport before RuntimeRequests.
    wrapper_capabilities.extend(
        ["async_hook_result", "rewind_files", "manager_sessions"]
            .into_iter()
            .map(str::to_owned),
    );
    if services.browsers.is_some() {
        wrapper_capabilities.push("manager_browsers".into());
    }
    let sink = Arc::new(LocalText(
        std::sync::Mutex::new(LocalOutput::default()),
        Notify::new(),
        wrapper_capabilities,
        std::sync::Mutex::new(std::collections::HashSet::new()),
    ));
    let connection = if let Some(automation) = &automation {
        LocalConnection::Native(automation.host.turn.connect(
            sink.clone(),
            token,
            env!("CARGO_PKG_VERSION").into(),
        ))
    } else {
        LocalConnection::Legacy(GatewayConnection::new(
            sink.clone(),
            gateway,
            token,
            env!("CARGO_PKG_VERSION").into(),
        ))
    };
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut decoder = Decoder::default();
    let mut authenticated = false;
    let mut sessions = std::collections::HashSet::new();
    let mut notifications = automation.as_ref().map(|a| a.subscribe());
    let mut pending: VecDeque<Call<'_, Result<(), RequestError>>> = VecDeque::new();
    let result = async {
        loop {
            while let Some(frame) = decoder
                .next()
                .map_err(|_| std::io::Error::other("invalid client websocket frame"))?
            {
                match acceptor::action(&frame) {
                    Action::Text => {
                        let parsed = dispatch::parse(&frame.payload);
                        if !authenticated && parsed.as_ref().is_some_and(dispatch::is_hello) {
                            hello = frame.wire.to_vec();
                        }
                        if authenticated && let (Some(fallback), Some(request)) = (&fallback, parsed.as_ref())
                            && dispatch::is_request(request) && should_forward(request, automation.is_some()) {
                            upstream_turns.request(request);
                            fallback.send(crate::runtime::session::routes::passthrough::Forward {
                                wire: frame.wire.to_vec(), id: request.get("id").expect("validated id").clone(), hello: hello.clone(),
                            })?;
                            continue;
                        }
                        if authenticated && !sink.0.lock().unwrap().closed
                            && let Some(request) = &parsed
                            && crate::metadata::text_field(request,"method").as_deref() == Some("submit_turn")
                            && let Some(session) = request.get("params").and_then(|p| crate::metadata::text_field(p,"sessionKey")) {
                            sessions.insert(session);
                        }
                        if authenticated && parsed.as_ref().is_some_and(dispatch::is_request) && parsed.as_ref().is_some_and(is_automation) {
                            let request=parsed.expect("validated automation request");
                            let automation=automation.clone(); let sink=sink.clone();
                            pending.push_back(Box::pin(async move { sink.send_text(String::from_utf8(automation_response(automation,&request).await).expect("JSON response")); Ok(()) }));
                        } else if authenticated && parsed.as_ref().and_then(|r|crate::metadata::text_field(r,"method")).is_some_and(|m|crate::routes::domains::METHODS.contains(&m.as_str())) {
                            let request = parsed.expect("domain request");
                            let metadata = metadata.clone(); let sink=sink.clone();
                            pending.push_back(Box::pin(async move {
                                let metadata=metadata.ok_or_else(||RequestError::new("PILOT_HOME is required for native management."))?;
                                let Route::LocalResponse(bytes)=crate::routes::domains::response(metadata,&request).await else { unreachable!("owned domain response") };
                                sink.send_text(String::from_utf8(bytes).expect("JSON response")); Ok(())
                            }));
                        } else if authenticated
                            && parsed.as_ref().is_some_and(crate::routes::native_request)
                        {
                            let request = parsed.expect("validated native request");
                            let services = services.clone();
                            let sink = sink.clone();
                            pending.push_back(Box::pin(async move {
                                let Route::LocalResponse(bytes) =
                                    crate::routes::native_route(&services, &request).await
                                else {
                                    unreachable!("native method registration");
                                };
                                sink.send_text(String::from_utf8(bytes).expect("native JSON"));
                                Ok(())
                            }));
                        } else if authenticated
                            && parsed.as_ref().is_some_and(dispatch::is_request)
                            && parsed.as_ref().is_some_and(dispatch::is_local)
                            && !(connection.is_native() && parsed.as_ref().and_then(|r|crate::metadata::text_field(r,"method")).is_some_and(|m| crate::runtime::mcp::routes::McpRoutes::handles(&m) || crate::runtime::turn::wiring::NATIVE_METHODS.contains(&m.as_str())))
                            && !matches!(
                                parsed
                                    .as_ref()
                                    .and_then(|request| crate::metadata::text_field(
                                        request, "method"
                                    ))
                                    .as_deref(),
                                Some("permission_decide" | "elicitation_respond")
                            )
                        {
                            let request = parsed.expect("validated local request");
                            let metadata = metadata.clone();
                            let mcp = mcp.clone();
                            let sink = sink.clone();
                            let interactions = interactions.clone();
                            let dialogs = dialogs.clone();
                            pending.push_back(Box::pin(async move {
                                let route = if crate::routes::is_mcp(&request) {
                                    let Route::LocalResponse(payload) =
                                        crate::routes::route_mcp(&request, mcp.as_deref()).await
                                    else {
                                        unreachable!("registered MCP method answers locally");
                                    };
                                    String::from_utf8(payload).expect("JSON response")
                                } else if matches!(
                                    crate::metadata::text_field(&request, "method").as_deref(),
                                    Some(
                                        "user_dialog_list"
                                            | "user_dialog_claim"
                                            | "user_dialog_release"
                                            | "user_dialog_respond"
                                    )
                                ) {
                                    let Route::LocalResponse(payload) =
                                        crate::routes::user_dialog(&request, &dialogs)
                                            .await
                                            .expect("owned dialog method")
                                    else {
                                        unreachable!("owned dialog response")
                                    };
                                    String::from_utf8(payload).expect("JSON response")
                                } else if dispatch::needs_files(&request) {
                                    tokio::task::spawn_blocking(move || {
                                        local_route(&request, metadata.as_deref(), &interactions)
                                    })
                                    .await
                                    .map_err(|_| RequestError::new("Local metadata task failed."))?
                                } else {
                                    local_route(&request, metadata.as_deref(), &interactions)
                                };
                                sink.send_text(route);
                                Ok(())
                            }));
                        } else {
                            let message = String::from_utf8_lossy(&frame.payload);
                            // receive validates/publishes authentication synchronously,
                            // before its describe_server future is polled.
                            if let Some(request) = &parsed && crate::metadata::text_field(request, "method").as_deref() == Some("describe_server") && let Some(id) = request.get("id").and_then(pilotdeck_protocol::JsValue::as_string).and_then(|id| id.to_utf8().ok()) { sink.3.lock().unwrap().insert(id); }
                            let call = connection.receive(&message);
                            if connection.is_native() && (!authenticated || !parsed.as_ref().is_some_and(dispatch::is_request)) {
                                call.await.map_err(|e|std::io::Error::other(String::from_utf16_lossy(e.message.units())))?;
                                authenticated = parsed.as_ref().is_some_and(dispatch::is_hello) && !sink.0.lock().unwrap().closed;
                            } else {
                                if !authenticated && parsed.as_ref().is_some_and(dispatch::is_hello) && !sink.0.lock().unwrap().closed {authenticated=true;}
                                pending.push_back(call);
                            }
                        }
                    }
                    Action::Ping => {
                        let mut output = sink.0.lock().unwrap();
                        if !output.closed {
                            output
                                .frames
                                .push_back(frame::server_frame(10, &frame.payload));
                        }
                    }
                    Action::Close => {
                        sink.close(1000, "");
                        break;
                    }
                    Action::Ignore => {}
                }
            }
            // Synchronous ping/close output precedes promises from this batch.
            loop {
                let next = sink.0.lock().unwrap().frames.pop_front();
                let Some(next) = next else { break };
                writer.write_all(&next).await?;
            }
            if sink.0.lock().unwrap().closed {
                writer.shutdown().await?;
                return Ok(());
            }
            tokio::select! {
                biased;
                result = poll_fn(|cx| {
                    for index in 0..pending.len() {
                        if let Poll::Ready(result) = pending[index].as_mut().poll(cx) {
                            drop(pending.remove(index));
                            return Poll::Ready(result);
                        }
                    }
                    Poll::Pending
                }), if !pending.is_empty() => {
                    result.map_err(|_| std::io::Error::other("local gateway request failed"))?;
                }
                notice = config_notices.recv() => {
                    if authenticated && let Ok((home, cwd, notice)) = notice
                        && metadata.as_ref().is_some_and(|m| m.home == home && m.cwd == cwd) {
                        sink.send_text(pilotdeck_protocol::stringify_json(&notice).unwrap().unwrap());
                    }
                }
                result = reader.read_buf(&mut decoder.buffer) => {
                    if result? == 0 { return Ok(()); }
                }
                _ = sink.1.notified() => {},
                wire = upstream_output.recv(), if fallback.is_some() => {
                    if let Some(wire) = wire { sink.0.lock().unwrap().frames.push_back(wire); }
                }
                event=receive_notification(&mut notifications), if notifications.is_some() => {
                    if authenticated && let Some(event)=event { sink.send_text(String::from_utf8(notification_bytes(event)).expect("JSON notification")); }
                }
            }
        }
    }
    .await;
    sink.0.lock().unwrap().closed = true;
    let _ = connection.closed().await;
    if let Some(fallback) = fallback {
        fallback.close().await;
    }
    // Keep already-dispatched work alive on close, as TS promises do. The
    // closed sink suppresses its output; no request task is detached.
    for call in pending {
        let _ = call.await;
    }
    if let Some(automation) = &automation {
        for session in sessions {
            automation.activity.disconnect(&format!("native:{session}"));
        }
    }
    result
}

fn should_forward(request: &pilotdeck_protocol::JsValue, legacy_submit: bool) -> bool {
    let method = crate::metadata::text_field(request, "method").unwrap_or_default();
    // Session-only submit is the legacy upstream control surface; native turns carry input.
    if method == "submit_turn" {
        return legacy_submit
            && request
                .get("params")
                .and_then(pilotdeck_protocol::JsValue::as_object)
                .is_some_and(|p| {
                    p.entries().len() == 1
                        && p.get("sessionKey")
                            .and_then(pilotdeck_protocol::JsValue::as_string)
                            .is_some()
                });
    }
    !dispatch::is_local(request)
        && !crate::routes::native_request(request)
        && !is_automation(request)
        && !crate::runtime::session::routes::METHODS.contains(&method.as_str())
        && !crate::runtime::turn::wiring::NATIVE_METHODS.contains(&method.as_str())
}

enum LocalConnection {
    Legacy(crate::runtime::connection::GatewayConnection),
    Native(crate::runtime::turn::gateway::HostedConnection),
}
impl LocalConnection {
    fn is_native(&self) -> bool {
        matches!(self, Self::Native(_))
    }
    fn receive(
        &self,
        message: &str,
    ) -> crate::runtime::connection::Call<'_, Result<(), crate::runtime::connection::RequestError>>
    {
        match self {
            Self::Legacy(connection) => connection.receive(message),
            Self::Native(connection) => Box::pin(connection.receive(message.to_owned())),
        }
    }
    fn closed(
        &self,
    ) -> crate::runtime::connection::Call<'_, Result<(), crate::runtime::connection::RequestError>>
    {
        match self {
            Self::Legacy(connection) => connection.closed(),
            Self::Native(connection) => Box::pin(connection.closed()),
        }
    }
}

#[derive(Default)]
struct LocalOutput {
    frames: VecDeque<Vec<u8>>,
    closed: bool,
}
struct LocalText(
    std::sync::Mutex<LocalOutput>,
    Notify,
    Vec<String>,
    std::sync::Mutex<std::collections::HashSet<String>>,
);

fn merge_bound_capabilities(message: String, bound: &[String], describe_response: bool) -> String {
    if bound.is_empty() {
        return message;
    }
    let Ok(mut frame) = serde_json::from_str::<serde_json::Value>(&message) else {
        return message;
    };
    let target = if frame["type"] == "hello_ok" {
        frame.get_mut("serverInfo")
    } else if describe_response && frame["type"] == "response" && frame["ok"] == true {
        frame.get_mut("result")
    } else {
        None
    };
    let Some(capabilities) = target
        .and_then(|v| v.get_mut("capabilities"))
        .and_then(serde_json::Value::as_array_mut)
    else {
        return message;
    };
    for capability in bound {
        if !capabilities
            .iter()
            .any(|v| v.as_str() == Some(capability.as_str()))
        {
            let order = crate::runtime::turn::capabilities::ORDER;
            let index = order.iter().position(|name| *name == capability);
            let insert_at = index
                .and_then(|index| {
                    capabilities.iter().position(|existing| {
                        existing
                            .as_str()
                            .and_then(|name| order.iter().position(|item| *item == name))
                            .is_some_and(|other| other > index)
                    })
                })
                .unwrap_or(capabilities.len());
            capabilities.insert(insert_at, capability.clone().into());
        }
    }
    serde_json::to_string(&frame).unwrap_or(message)
}

#[cfg(test)]
mod wrapper_capability_tests {
    use super::merge_bound_capabilities;
    #[test]
    fn merge_advertises_only_supplied_ports_and_keeps_existing_order() {
        let input =
            r#"{"type":"hello_ok","serverInfo":{"capabilities":["native","permission_list"]}}"#;
        let output: serde_json::Value = serde_json::from_str(&merge_bound_capabilities(
            input.into(),
            &["permission_list".into(), "user_dialog_list".into()],
            false,
        ))
        .unwrap();
        assert_eq!(
            output["serverInfo"]["capabilities"],
            serde_json::json!(["native", "user_dialog_list", "permission_list"])
        );
        assert_eq!(merge_bound_capabilities(input.into(), &[], false), input);
        assert!(!output.to_string().contains("always_on_apply"));
        let business = r#"{"type":"response","ok":true,"result":{"capabilities":["business"]}}"#;
        assert_eq!(
            merge_bound_capabilities(business.into(), &["wrapper".into()], false),
            business
        );
        let described: serde_json::Value = serde_json::from_str(&merge_bound_capabilities(
            business.into(),
            &["wrapper".into()],
            true,
        ))
        .unwrap();
        assert_eq!(
            described["result"]["capabilities"],
            serde_json::json!(["business", "wrapper"])
        );
    }
}
impl crate::runtime::connection::GatewayTextConnection for LocalText {
    fn send_text(&self, message: String) {
        let describe_response = serde_json::from_str::<serde_json::Value>(&message)
            .ok()
            .and_then(|frame| {
                frame
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|id| self.3.lock().unwrap().remove(&id));
        let message = merge_bound_capabilities(message, &self.2, describe_response);
        let mut output = self.0.lock().unwrap();
        if !output.closed {
            output
                .frames
                .push_back(frame::server_frame(1, message.as_bytes()));
            self.1.notify_one();
        }
    }
    fn close(&self, code: u16, reason: &str) {
        let mut output = self.0.lock().unwrap();
        if !output.closed {
            output.closed = true;
            let mut payload = code.to_be_bytes().to_vec();
            payload.extend_from_slice(reason.as_bytes());
            output.frames.push_back(frame::server_frame(8, &payload));
        }
    }
}

fn local_route<F: crate::metadata::Files>(
    request: &pilotdeck_protocol::JsValue,
    metadata: Option<&crate::metadata::Metadata<F>>,
    interactions: &GatewayInteractionCoordinator,
) -> String {
    use crate::metadata::object;
    use pilotdeck_protocol::{JsValue, stringify_json};
    let payload = match crate::routes::route_with_interactions(request, metadata, interactions) {
        Route::LocalResponse(payload) => return String::from_utf8(payload).expect("JSON response"),
        Route::Forward => object([
            ("type", "response".into()),
            ("id", request.get("id").expect("validated request").clone()),
            ("ok", JsValue::Bool(false)),
            (
                "error",
                object([
                    ("code", "CAPABILITY_UNAVAILABLE".into()),
                    (
                        "message",
                        "Request is unavailable on this local gateway.".into(),
                    ),
                ]),
            ),
        ]),
    };
    stringify_json(&payload).unwrap().unwrap()
}

struct LocalGateway {
    services: Arc<crate::routes::GatewayServices>,
    interactions: Arc<GatewayInteractionCoordinator>,
}
impl LocalGateway {
    fn info(&self) -> pilotdeck_protocol::JsValue {
        use crate::metadata::object;
        object([
            ("mode", "in_process".into()),
            (
                "protocolVersion",
                pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION.into(),
            ),
            (
                "capabilities",
                pilotdeck_protocol::JsValue::Array(
                    [
                        "async_hook_result",
                        "rewind_files",
                        "manager_sessions",
                        "manager_browsers",
                    ]
                    .into_iter()
                    .filter(|name| *name != "manager_browsers" || self.services.browsers.is_some())
                    .map(Into::into)
                    .collect(),
                ),
            ),
        ])
    }
}
impl crate::runtime::connection::ConnectionGateway for LocalGateway {
    fn describe_server(
        &self,
    ) -> crate::runtime::connection::Call<
        '_,
        Result<pilotdeck_protocol::JsValue, crate::runtime::connection::RequestError>,
    > {
        Box::pin(async { Ok(self.info()) })
    }
    fn request(
        &self,
        method: pilotdeck_protocol::JsString,
        params: pilotdeck_protocol::JsValue,
    ) -> crate::runtime::connection::Call<
        '_,
        Result<pilotdeck_protocol::JsValue, crate::runtime::connection::RequestError>,
    > {
        Box::pin(async move {
            if let Ok(name) = method.to_utf8()
                && let Some(result) = self.services.request(&name, &params).await
            {
                return result;
            }
            if method.equals_str("describe_server") {
                return Ok(self.info());
            }
            if method.equals_str("permission_decide") || method.equals_str("elicitation_respond") {
                let method = if method.equals_str("permission_decide") {
                    "permission_decide"
                } else {
                    "elicitation_respond"
                };
                return crate::runtime::permission::routes::handle(
                    method,
                    &params,
                    &self.interactions,
                )
                .or_else(|| {
                    crate::runtime::interaction::routes::handle(method, &params, &self.interactions)
                })
                .expect("owned interaction method");
            }
            let mut message = pilotdeck_protocol::JsString::from("Unknown gateway method ")
                .units()
                .to_vec();
            message.extend_from_slice(method.units());
            message.push(b'.' as u16);
            Err(crate::runtime::connection::RequestError::new(
                pilotdeck_protocol::JsString::from_utf16(message),
            ))
        })
    }
    fn submit<'a>(
        &'a self,
        _params: pilotdeck_protocol::JsValue,
        _emit: &'a (dyn Fn(pilotdeck_protocol::JsValue) + Send + Sync),
    ) -> crate::runtime::connection::Call<'a, Result<(), crate::runtime::connection::RequestError>>
    {
        Box::pin(async {
            Err(crate::runtime::connection::RequestError::new(
                "submit_turn is unavailable on this local gateway.",
            ))
        })
    }
    fn disconnect(
        &self,
        _session: pilotdeck_protocol::JsValue,
        _binding: crate::runtime::connection::binding::WsBinding,
    ) -> crate::runtime::connection::Call<'_, Result<bool, crate::runtime::connection::RequestError>>
    {
        Box::pin(async { Ok(false) })
    }
    fn abort_on_close(
        &self,
        _params: pilotdeck_protocol::JsValue,
    ) -> crate::runtime::connection::Call<'_, Result<(), crate::runtime::connection::RequestError>>
    {
        Box::pin(async { Ok(()) })
    }
}

fn is_automation(request: &pilotdeck_protocol::JsValue) -> bool {
    crate::metadata::text_field(request, "method")
        .is_some_and(|m| crate::cron::Automation::handles(&m))
}
async fn automation_response(
    automation: Option<Arc<crate::cron::Automation>>,
    request: &pilotdeck_protocol::JsValue,
) -> Vec<u8> {
    let id = request
        .get("id")
        .cloned()
        .unwrap_or(pilotdeck_protocol::JsValue::Null);
    if automation.is_none() {
        let method = crate::metadata::text_field(request, "method").unwrap_or_default();
        let (message, code) = match method.as_str() {
            "background_task_stop" => (
                "background_task_stop is unavailable.",
                "CAPABILITY_UNAVAILABLE",
            ),
            "background_tasks" => ("background_tasks is unavailable.", "CAPABILITY_UNAVAILABLE"),
            _ => (
                "Native automation requires an explicit PILOT_HOME and sidecar executable.",
                "AUTOMATION_UNAVAILABLE",
            ),
        };
        let value = crate::metadata::object([
            ("type", "response".into()),
            ("id", id),
            ("ok", pilotdeck_protocol::JsValue::Bool(false)),
            (
                "error",
                crate::metadata::object([("code", code.into()), ("message", message.into())]),
            ),
        ]);
        return pilotdeck_protocol::stringify_json(&value)
            .expect("JSON error")
            .expect("error")
            .into_bytes();
    }
    automation
        .expect("automation present after availability gate")
        .response(request)
        .await
}
async fn receive_notification(
    receiver: &mut Option<tokio::sync::broadcast::Receiver<serde_json::Value>>,
) -> Option<serde_json::Value> {
    let Some(receiver) = receiver else {
        return std::future::pending().await;
    };
    match receiver.recv().await {
        Ok(event) => Some(event),
        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
            Some(serde_json::json!({"name":"automation:event-gap","payload":{"dropped":count}}))
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => std::future::pending().await,
    }
}
fn notification_bytes(event: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"type":"notification","name":event["name"],"payload":event["payload"]}),
    )
    .expect("notification JSON")
}

#[cfg(test)]
mod local_tests {
    use super::*;
    use pilotdeck_protocol::JsValue;
    use tokio::time::{Duration, timeout};

    fn client(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let length_code = match payload.len() {
            0..126 => payload.len() as u8,
            126..=65535 => 126,
            _ => 127,
        };
        frame::client_frame(0x80 | opcode, length_code, payload)
    }

    fn hello(token: &str) -> Vec<u8> {
        client(1, format!(r#"{{"type":"hello","protocolVersion":"1.2","clientName":"g0","clientVersion":"1","token":"{token}"}}"#).as_bytes())
    }
    async fn exchange(parts: Vec<Vec<u8>>, token: String) -> Vec<frame::Frame> {
        exchange_with_mcp(parts, token, None).await
    }
    async fn exchange_with_mcp(
        parts: Vec<Vec<u8>>,
        token: String,
        mcp: Option<Arc<McpRoutes>>,
    ) -> Vec<frame::Frame> {
        timeout(Duration::from_secs(3), async {
            let (server, mut client) = tokio::io::duplex(16384);
            let job = tokio::spawn(run_local(server, token, mcp));
            client.write_all(&parts.concat()).await.unwrap();
            client.shutdown().await.unwrap();
            let mut decoder = Decoder::default();
            let mut bytes = Vec::new();
            client.read_to_end(&mut bytes).await.unwrap();
            decoder.buffer.extend_from_slice(&bytes);
            job.await.unwrap().unwrap();
            let mut frames = Vec::new();
            while let Some(frame) = decoder.next_server().unwrap() {
                frames.push(frame);
            }
            assert!(decoder.buffer.is_empty());
            frames
        })
        .await
        .expect("local transport timeout")
    }
    fn text(raw: &str) -> Vec<u8> {
        client(1, raw.as_bytes())
    }

    #[tokio::test]
    async fn local_mcp_status_uses_shared_sdk_across_requests_and_connections() {
        let (sdk, mcp) = crate::runtime::mcp::routes::tests::fixture();
        let json = |text: &str| pilotdeck_protocol::parse_json(text).unwrap();
        let configured = sdk
            .set_mcp_servers(&json(
                r#"{"sessionKey":"s","servers":{"owned":{"type":"stdio","command":"unused"}}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(
            configured,
            json(r#"{"added":["owned"],"removed":[],"errors":[]}"#)
        );
        for (id, expected) in [("first", "configured"), ("second", "disabled")] {
            if id == "second" {
                sdk.toggle_mcp_server(&json(
                    r#"{"sessionKey":"s","serverName":"owned","enabled":false}"#,
                ))
                .await
                .unwrap();
            }
            let token = uuid::Uuid::new_v4().to_string();
            let frames = exchange_with_mcp(
                vec![hello(&token), text(&format!(r#"{{"type":"request","id":"{id}","method":"mcp_server_status","params":{{"sessionKey":"s"}}}}"#)),
                    text(r#"{"type":"request","id":"other","method":"mcp_server_status","params":{"sessionKey":"other"}}"#)],
                token,
                Some(mcp.clone()),
            ).await;
            assert_eq!(frames.len(), 3);
            assert_eq!(
                dispatch::parse(&frames[1].payload).unwrap(),
                json(&format!(
                    r#"{{"type":"response","id":"{id}","ok":true,"result":{{"servers":[{{"name":"plugin","status":"configured"}},{{"name":"owned","status":"{expected}"}}]}}}}"#
                ))
            );
            assert_eq!(
                dispatch::parse(&frames[2].payload).unwrap(),
                json(r#"{"type":"response","id":"other","ok":true,"result":{"servers":[]}}"#)
            );
        }
    }

    #[tokio::test]
    async fn local_mcp_status_errors_preserve_missing_params_and_capability_precedence() {
        let (_, mcp) = crate::runtime::mcp::routes::tests::fixture();
        for (params, kind) in [("", "undefined"), (r#", "params":null"#, "null")] {
            let token = uuid::Uuid::new_v4().to_string();
            let request =
                format!(r#"{{"type":"request","id":"e","method":"mcp_server_status"{params}}}"#);
            let frames = exchange_with_mcp(
                vec![hello(&token), text(&request)],
                token,
                Some(mcp.clone()),
            )
            .await;
            let reply = dispatch::parse(&frames[1].payload).unwrap();
            let error = reply.get("error").unwrap();
            assert_eq!(reply.get("ok"), Some(&JsValue::Bool(false)));
            assert_eq!(error.get("code"), Some(&"gateway_request_failed".into()));
            assert_eq!(
                crate::metadata::text_field(error, "message").unwrap(),
                format!("Cannot read properties of {kind} (reading 'projectKey')")
            );
        }
        let token = uuid::Uuid::new_v4().to_string();
        let frames = exchange(
            vec![
                hello(&token),
                text(r#"{"type":"request","id":"e","method":"mcp_server_status"}"#),
            ],
            token,
        )
        .await;
        let reply = dispatch::parse(&frames[1].payload).unwrap();
        assert_eq!(
            reply.get("error").unwrap().get("code"),
            Some(&"CAPABILITY_UNAVAILABLE".into())
        );
    }

    #[tokio::test]
    async fn local_hello_and_supported_agents_do_not_need_upstream() {
        let token = uuid::Uuid::new_v4().to_string();
        let frames = exchange(
            vec![
                hello(&token),
                text(r#"{"type":"request","id":"1","method":"supported_agents"}"#),
                text(r#"{"type":"request","id":"2","method":"describe_server"}"#),
            ],
            token,
        )
        .await;
        assert_eq!(frames.len(), 3);
        let hello = dispatch::parse(&frames[0].payload).unwrap();
        assert_eq!(
            crate::metadata::text_field(&hello, "type").as_deref(),
            Some("hello_ok")
        );
        let binding = hello.get("interactionBinding").unwrap();
        let id = crate::metadata::text_field(binding, "connectionId").unwrap();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert!(matches!(binding.get("generation"), Some(JsValue::Number(n)) if *n > 0.0));
        let agents = dispatch::parse(&frames[1].payload).unwrap();
        assert_eq!(agents.get("ok"), Some(&JsValue::Bool(true)));
        let describe = dispatch::parse(&frames[2].payload).unwrap();
        assert_eq!(hello.get("serverInfo"), describe.get("result"));
    }

    #[tokio::test]
    async fn local_authentication_closes_with_ts_codes_and_reasons() {
        for (input, expected) in [
            ("{", (4002u16, "invalid_json")),
            (
                r#"{"type":"request","id":"1","method":"supported_agents"}"#,
                (4001, "hello_required"),
            ),
            (
                r#"{"type":"hello","protocolVersion":"wrong","clientName":"g0","clientVersion":"1","token":"wrong"}"#,
                (4001, "protocol_mismatch"),
            ),
            (
                r#"{"type":"hello","protocolVersion":"1.2","clientName":"g0","clientVersion":"1","token":"wrong"}"#,
                (4003, "auth_failed"),
            ),
            (
                r#"{"type":"hello","protocolVersion":"1.2","clientName":"g0","clientVersion":1,"token":"wrong"}"#,
                (4001, "hello_required"),
            ),
        ] {
            let frames = exchange(vec![text(input)], uuid::Uuid::new_v4().to_string()).await;
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].opcode, 8);
            assert_eq!(
                u16::from_be_bytes(frames[0].payload[..2].try_into().unwrap()),
                expected.0
            );
            assert_eq!(&frames[0].payload[2..], expected.1.as_bytes());
        }
    }

    #[tokio::test]
    async fn local_batch_ping_precedes_async_text_and_close_suppresses_pending() {
        let token = uuid::Uuid::new_v4().to_string();
        let request = text(r#"{"type":"request","id":"1","method":"supported_agents"}"#);
        let ping = client(9, b"ping-data");
        let frames = exchange(
            vec![hello(&token), request.clone(), ping.clone()],
            token.clone(),
        )
        .await;
        assert_eq!(
            frames.iter().map(|f| f.opcode).collect::<Vec<_>>(),
            [10, 1, 1]
        );
        assert_eq!(frames[0].payload, b"ping-data");
        let frames = exchange(
            vec![hello(&token), request, ping, client(8, b""), text("{")],
            token,
        )
        .await;
        assert_eq!(frames.iter().map(|f| f.opcode).collect::<Vec<_>>(), [10, 8]);
        assert_eq!(frames[1].payload, 1000u16.to_be_bytes());
    }

    #[tokio::test]
    async fn local_invalid_authed_frame_closes_before_pending_hello() {
        let token = uuid::Uuid::new_v4().to_string();
        let frames = exchange(vec![hello(&token), text("null")], token).await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].opcode, 8);
        assert_eq!(&frames[0].payload[2..], b"invalid_frame");
    }

    #[tokio::test]
    async fn local_bad_hello_does_not_pong_a_coalesced_ping() {
        let frames = exchange(
            vec![
                text(r#"{"type":"hello","protocolVersion":"1.2","clientName":"g0","clientVersion":"1","token":"wrong"}"#),
                client(9, b"after-close"),
            ],
            uuid::Uuid::new_v4().to_string(),
        )
        .await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].opcode, 8);
        assert_eq!(&frames[0].payload[2..], b"auth_failed");
    }
}
