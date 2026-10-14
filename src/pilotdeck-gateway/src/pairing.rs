//! Two pumps share a downstream writer so pong/close cannot interleave bytes.
//! The pair owns both streams; either pump completing drops both, without a
//! detached read task or waiting forever for the peer to finish a half-close.
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

pub(crate) async fn run<D, U>(downstream: D, upstream: U) -> std::io::Result<()>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (down_read, down_write) = tokio::io::split(downstream);
    let (mut up_read, up_write) = tokio::io::split(upstream);
    let down_write = Mutex::new(down_write);
    let receive = async {
        let mut decoder = Decoder::default();
        loop {
            match decoder.next_server() {
                Ok(Some(frame)) => {
                    // Hold the writer for an entire frame, not a TCP chunk:
                    // a local pong must never land inside a TS text payload.
                    down_write.lock().await.write_all(&frame.wire).await?;
                }
                Ok(None) => {
                    if up_read.read_buf(&mut decoder.buffer).await? == 0 {
                        // Preserve a truncated upstream tail too, then close.
                        down_write.lock().await.write_all(&decoder.buffer).await?;
                        return Ok(());
                    }
                }
                Err(_) => {
                    // Upstream owns its wire even for malformed output. Keep
                    // the lock during fallback so locally generated replies
                    // cannot corrupt an unparseable frame in flight.
                    let mut writer = down_write.lock().await;
                    writer.write_all(&decoder.buffer).await?;
                    tokio::io::copy(&mut up_read, &mut *writer).await?;
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

async fn send<R, U, D>(mut reader: R, mut writer: U, downstream: &Mutex<D>) -> std::io::Result<()>
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
                    Route::Forward => upstream::forward(&mut writer, &frame).await?,
                    Route::LocalResponse(payload) => {
                        downstream
                            .lock()
                            .await
                            .write_all(&frame::server_frame(1, &payload))
                            .await?
                    }
                },
                Action::Pong(wire) => downstream.lock().await.write_all(&wire).await?,
                Action::Close(wire) => {
                    downstream.lock().await.write_all(&wire).await?;
                    // TS must observe disconnect on its own dedicated endpoint.
                    upstream::forward(&mut writer, &frame).await?;
                    return Ok(());
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

#[cfg(test)]
mod tests;
