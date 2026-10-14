//! TS owns pong/close and their ordering while the local whitelist is empty.
//! Client close stops the send pump but leaves upstream output draining to EOF.
//! Upstream write failure also drains its pending output; other EOF/error paths
//! cancel the peer pump. Neither pump is detached.
use crate::{
    acceptor::{self, Action},
    dispatch::{self, Route},
    frame::{self, Decoder},
    upstream,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Mutex,
};

struct Downstream<W> {
    writer: W,
    closed: bool,
}

pub(crate) async fn run<D, U>(downstream: D, upstream: U) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (down_read, down_write) = tokio::io::split(downstream);
    let (mut up_read, up_write) = tokio::io::split(upstream);
    let down_write = Mutex::new(Downstream {
        writer: down_write,
        closed: false,
    });
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
        result = send(down_read, up_write, &down_write) => result,
        result = receive => result,
    }
}

async fn send<R, U, D>(
    mut reader: R,
    mut writer: U,
    downstream: &Mutex<Downstream<D>>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    U: AsyncWrite + Unpin,
    D: AsyncWrite + Unpin,
{
    let mut decoder = Decoder::default();
    loop {
        match decoder.next() {
            Ok(Some(frame)) => match acceptor::action(&frame) {
                Action::Text => match dispatch::route(&frame.payload) {
                    Route::Forward => forward_or_drain(&mut writer, &frame).await,
                    Route::LocalResponse(payload) => {
                        let mut downstream = downstream.lock().await;
                        if !downstream.closed {
                            downstream
                                .writer
                                .write_all(&frame::server_frame(1, &payload))
                                .await?;
                        }
                    }
                },
                Action::Ping => forward_or_drain(&mut writer, &frame).await,
                Action::Close => {
                    forward_or_drain(&mut writer, &frame).await;
                    // TS returns from handleData on close, ignoring subsequent
                    // coalesced frames. Keep both streams alive until receive
                    // drains TS's queued event/response/close and reaches EOF.
                    return std::future::pending().await;
                }
                Action::Ignore => {}
            },
            Ok(None) => {
                if reader.read_buf(&mut decoder.buffer).await? == 0 {
                    // TS likewise produces no protocol close for a truncated
                    // frame or TCP EOF. Returning cancels the upstream pump.
                    return Ok(());
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
