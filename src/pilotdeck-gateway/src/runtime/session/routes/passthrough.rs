//! The SESSION boundary forwards only requests outside its registered methods.
use crate::{
    frame,
    metadata::{object, text_field},
    upstream,
};
use hyper::{HeaderMap, Method};
use hyper_util::rt::TokioIo;
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    task::JoinHandle,
};

pub(crate) struct Upgrade {
    pub origin: String,
    pub path: String,
    pub method: Method,
    pub headers: HeaderMap,
    pub accept: String,
}

pub(crate) struct Forward {
    pub wire: Vec<u8>,
    pub id: JsValue,
    pub hello: Vec<u8>,
}

/// Observer supplied by the owning channel transport. The runtime has no dependency on channel configuration.
pub trait UpstreamTurnObserver: Send + Sync {
    fn response(&self, value: &JsValue);
}

pub(crate) struct Passthrough {
    send: mpsc::UnboundedSender<Forward>,
    task: JoinHandle<()>,
}

impl Passthrough {
    pub fn start(upgrade: Upgrade, outbound: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self::with_turns(upgrade, outbound, None)
    }
    pub fn with_turns(
        upgrade: Upgrade,
        outbound: mpsc::UnboundedSender<Vec<u8>>,
        turns: Option<std::sync::Arc<dyn UpstreamTurnObserver>>,
    ) -> Self {
        let (send, mut receive) = mpsc::unbounded_channel::<Forward>();
        let task = tokio::spawn(async move {
            // SESSION never waits on upstream availability, including its handshake.
            while let Some(first) = receive.recv().await {
                let ready = tokio::time::timeout(Duration::from_secs(10), async {
                    let (stream, _) = upstream::connect(
                        &upgrade.origin,
                        &upgrade.path,
                        &upgrade.method,
                        &upgrade.headers,
                        &upgrade.accept,
                    )
                    .await
                    .map_err(std::io::Error::other)?;
                    let mut stream = TokioIo::new(stream);
                    stream.write_all(&first.hello).await?;
                    let mut decoder = frame::Decoder::default();
                    loop {
                        if let Some(frame) = decoder
                            .next_server()
                            .map_err(|e| std::io::Error::other(format!("{e:?}")))?
                        {
                            if frame.opcode == 1
                                && parse_json(&String::from_utf8_lossy(&frame.payload))
                                    .ok()
                                    .is_some_and(|value| {
                                        text_field(&value, "type").as_deref() == Some("hello_ok")
                                    })
                            {
                                return Ok::<_, std::io::Error>((stream, decoder));
                            }
                            if frame.opcode == 8 {
                                let _ = outbound.send(frame.wire.to_vec());
                                return Err(std::io::Error::other(
                                    "upstream authentication closed",
                                ));
                            }
                            let _ = outbound.send(frame.wire.to_vec());
                        } else if stream.read_buf(&mut decoder.buffer).await? == 0 {
                            return Err(std::io::Error::other("upstream authentication ended"));
                        }
                    }
                })
                .await;
                let (mut stream, mut decoder) = match ready {
                    Ok(Ok(ready)) => ready,
                    _ => {
                        unavailable(&outbound, first.id);
                        continue;
                    }
                };
                if stream.write_all(&first.wire).await.is_err() {
                    unavailable(&outbound, first.id);
                    continue;
                }
                let result = async {
                    loop {
                        while let Some(frame) = decoder.next_server().map_err(|e| std::io::Error::other(format!("{e:?}")))? {
                            if frame.opcode == 1 && let (Some(turns), Ok(value)) = (&turns, parse_json(&String::from_utf8_lossy(&frame.payload))) {
                                turns.response(&value);
                            }
                            let closed = frame.opcode == 8;
                            if outbound.send(frame.wire.to_vec()).is_err() || closed { return Ok::<_, std::io::Error>(()); }
                        }
                        tokio::select! {
                            next = receive.recv() => {
                                let Some(next) = next else { return Ok(()); };
                                stream.write_all(&next.wire).await?;
                            }
                            bytes = stream.read_buf(&mut decoder.buffer) => {
                                if bytes? == 0 { return Err(std::io::Error::other("upstream websocket ended")); }
                            }
                        }
                    }
                }.await;
                if result.is_err() {
                    let mut close = 1011u16.to_be_bytes().to_vec();
                    close.extend_from_slice(b"upstream websocket ended");
                    let _ = outbound.send(frame::server_frame(8, &close));
                }
                break;
            }
        });
        Self { send, task }
    }

    pub fn send(&self, request: Forward) -> std::io::Result<()> {
        self.send
            .send(request)
            .map_err(|_| std::io::Error::other("upstream websocket ended"))
    }

    pub async fn close(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

fn unavailable(outbound: &mpsc::UnboundedSender<Vec<u8>>, id: JsValue) {
    let response = object([
        ("type", "response".into()),
        ("id", id),
        ("ok", JsValue::Bool(false)),
        (
            "error",
            object([
                ("code", "gateway_request_failed".into()),
                ("message", "upstream websocket unavailable".into()),
            ]),
        ),
    ]);
    let payload = stringify_json(&response)
        .expect("response is JSON")
        .expect("response is defined");
    let _ = outbound.send(frame::server_frame(1, payload.as_bytes()));
}
