//! TS owns authentication, pong/close, and their ordering.
//! Client close stops the send pump but leaves upstream output draining to EOF.
//! Upstream write failure also drains its pending output; other EOF/error paths
//! cancel the peer pump. Neither pump is detached.
use crate::{
    acceptor::{self, Action},
    dispatch::{self, Route},
    frame::{self, Decoder},
    upstream,
};
use std::{collections::VecDeque, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Mutex, Notify},
    task::JoinSet,
};

struct Downstream<W> {
    writer: W,
    closed: bool,
    authenticated: bool,
    handshake_started: bool,
    invalidated: bool,
    pings_pending: usize,
}

pub(crate) async fn run<D, U>(downstream: D, upstream: U) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    run_with_metadata(downstream, upstream, crate::metadata::Metadata::from_env()?).await
}

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
    let metadata = metadata.map(Arc::new);
    let (down_read, down_write) = tokio::io::split(downstream);
    let (mut up_read, up_write) = tokio::io::split(upstream);
    let down_write = Mutex::new(Downstream {
        writer: down_write,
        closed: false,
        authenticated: false,
        handshake_started: false,
        invalidated: false,
        pings_pending: 0,
    });
    let ping_completed = Notify::new();
    let receive = async {
        let mut decoder = Decoder::default();
        loop {
            match decoder.next_server() {
                Ok(Some(frame)) => {
                    let mut downstream = down_write.lock().await;
                    if frame.opcode == 8 {
                        // Future local handlers must never synthesize output
                        // after TS closes, even before the upstream EOF arrives.
                        downstream.closed = true;
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
        result = send(down_read, up_write, &down_write, metadata.as_ref(), &ping_completed) => result,
        result = receive => result,
    }
}

async fn send<R, U, D, F>(
    mut reader: R,
    mut writer: U,
    downstream: &Mutex<Downstream<D>>,
    metadata: Option<&Arc<crate::metadata::Metadata<F>>>,
    ping_completed: &Notify,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    U: AsyncWrite + Unpin,
    D: AsyncWrite + Unpin,
    F: crate::metadata::Files + Send + Sync + 'static,
{
    let mut decoder = Decoder::default();
    let mut jobs = JoinSet::new();
    let mut ready = VecDeque::new();
    loop {
        match decoder.next() {
            Ok(Some(frame)) => match acceptor::action(&frame) {
                Action::Text => {
                    let request = dispatch::parse(&frame.payload);
                    let eligible = {
                        let mut state = downstream.lock().await;
                        if !state.handshake_started {
                            state.handshake_started = true;
                            if !request.as_ref().is_some_and(dispatch::is_hello) {
                                state.invalidated = true;
                            }
                        } else if !request.as_ref().is_some_and(dispatch::is_request) {
                            // This can be queued while hello_ok is in flight.
                            // TS is already authed when it sends hello_ok, so
                            // invalid JSON / nonrequests will close the pair.
                            state.invalidated = true;
                        }
                        state.authenticated && !state.invalidated && !state.closed
                    };
                    if eligible && request.as_ref().is_some_and(dispatch::is_local) {
                        let mut request = request.expect("eligible local request");
                        // This proxy fronts a `pilotdeck server` boot, whose
                        // fallbackProjectRoot is PILOT_HOME (pilotdeck.ts:107-111):
                        // a nullish archive projectKey reads the General
                        // workspace, not this process's cwd. Same shared fill
                        // as the isolated gate, with the proxy boot's fallback.
                        if let Some(metadata) = metadata {
                            crate::native_archive::fill_request_default_project_key(
                                &mut request,
                                &metadata.home,
                            );
                        }
                        if dispatch::needs_files(&request) {
                            let metadata = metadata.cloned();
                            // Completion is polled only after the current input
                            // buffer: TS dispatches promises without blocking
                            // its frame parser, so coalesced close/ping wins.
                            jobs.spawn_blocking(move || {
                                let route =
                                    dispatch::route_with_metadata(&request, metadata.as_deref());
                                (frame, route)
                            });
                        } else {
                            match dispatch::route_with_metadata(&request, metadata.map(Arc::as_ref))
                            {
                                Route::Forward => forward_or_drain(&mut writer, &frame).await,
                                route => ready.push_back((frame, route)),
                            }
                        }
                    } else {
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
                if can_emit && let Some((frame, route)) = ready.pop_front() {
                    emit_or_forward(&mut writer, downstream, frame, route).await?;
                    continue;
                }
                tokio::select! {
                    biased;
                    result = jobs.join_next(), if can_emit && !jobs.is_empty() => {
                        let (frame, route) = result.expect("nonempty jobs").map_err(std::io::Error::other)?;
                        emit_or_forward(&mut writer, downstream, frame, route).await?;
                    }
                    result = reader.read_buf(&mut decoder.buffer) => {
                        if result? == 0 { return Ok(()); }
                    }
                    _ = ping_completed.notified(), if !can_emit => {},
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

async fn emit_or_forward<U: AsyncWrite + Unpin, D: AsyncWrite + Unpin>(
    upstream: &mut U,
    downstream: &Mutex<Downstream<D>>,
    frame: frame::Frame,
    route: Route,
) -> std::io::Result<()> {
    match route {
        Route::Forward => forward_or_drain(upstream, &frame).await,
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

#[cfg(test)]
#[path = "../tests/fixtures/native-collation/regression.rs"]
mod native_collation_tests;
