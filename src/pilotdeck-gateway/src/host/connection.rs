//! One owned child and independent pipe pumps. No callback runs in a pipe pump.
use super::{HostError, HostErrorKind, Limits};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

enum WriteCommand {
    Frame(String),
    Flush(oneshot::Sender<()>),
}

pub(super) struct Connection {
    child: Child,
    outgoing: Option<mpsc::Sender<WriteCommand>>,
    incoming: mpsc::Receiver<Result<JsValue, HostError>>,
    pumps: Vec<JoinHandle<()>>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    limits: Limits,
    closed: bool,
}

impl Connection {
    pub(super) fn spawn(command: &mut Command, limits: Limits) -> Result<Self, HostError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(HostError::io)?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");
        let (outgoing, mut writes) = mpsc::channel(limits.queue_capacity);
        let (reads, incoming) = mpsc::channel(limits.queue_capacity);
        let writer_errors = reads.clone();
        let writer = tokio::spawn(async move {
            while let Some(command) = writes.recv().await {
                let result = match command {
                    WriteCommand::Frame(line) => stdin.write_all(line.as_bytes()).await,
                    WriteCommand::Flush(done) => match stdin.flush().await {
                        Ok(()) => {
                            let _ = done.send(());
                            Ok(())
                        }
                        Err(error) => Err(error),
                    },
                };
                if let Err(error) = result {
                    let _ = writer_errors.send(Err(HostError::io(error))).await;
                    break;
                }
            }
        });
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                // read_until alone permits an unbounded unterminated line.
                let available = match reader.fill_buf().await {
                    Ok([]) => {
                        if !line.is_empty() && !deliver(&reads, &line).await {
                            return;
                        }
                        let _ = reads
                            .send(Err(HostError::new(
                                HostErrorKind::Disconnected,
                                "Sidecar stdout closed before execute reached a terminal event.",
                            )))
                            .await;
                        return;
                    }
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let _ = reads.send(Err(HostError::io(error))).await;
                        return;
                    }
                };
                let size = available
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(available.len(), |i| i + 1);
                if line.len() + size > limits.max_frame_bytes {
                    let _ = reads
                        .send(Err(HostError::new(
                            HostErrorKind::Limit,
                            "Sidecar stdout frame exceeds configured byte limit.",
                        )))
                        .await;
                    return;
                }
                line.extend_from_slice(&available[..size]);
                reader.consume(size);
                if line.last() == Some(&b'\n') {
                    if !deliver(&reads, &line).await {
                        return;
                    }
                    line.clear();
                }
            }
        });
        // TS stdio connection: lines 81-84, 158-169; always drain, retain only tail.
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let retained = stderr.clone();
        let stderr_task = tokio::spawn(async move {
            let mut bytes = [0; 4096];
            while let Ok(size) = stderr_pipe.read(&mut bytes).await {
                if size == 0 {
                    break;
                }
                let mut tail = retained.lock().unwrap_or_else(|e| e.into_inner());
                tail.extend(&bytes[..size]);
                let excess = tail.len().saturating_sub(limits.max_stderr_bytes);
                tail.drain(..excess);
            }
        });
        Ok(Self {
            child,
            outgoing: Some(outgoing),
            incoming,
            pumps: vec![reader, writer, stderr_task],
            stderr,
            limits,
            closed: false,
        })
    }

    pub(super) fn send(&self, value: &JsValue) -> Result<(), HostError> {
        let mut line = stringify_json(value)
            .map_err(|e| HostError::protocol(e.to_string()))?
            .ok_or_else(|| HostError::protocol("Cannot send undefined."))?;
        line.push('\n');
        if line.len() > self.limits.max_frame_bytes {
            return Err(HostError::new(
                HostErrorKind::Limit,
                "Outgoing frame exceeds configured byte limit.",
            ));
        }
        self.enqueue(WriteCommand::Frame(line))
    }
    fn enqueue(&self, command: WriteCommand) -> Result<(), HostError> {
        self.outgoing
            .as_ref()
            .ok_or_else(|| HostError::protocol("Sidecar connection is closed."))?
            .try_send(command)
            .map_err(|error| {
                HostError::new(
                    HostErrorKind::Limit,
                    format!("Sidecar writer unavailable or queue full: {error}"),
                )
            })
    }
    pub(super) async fn receive(&mut self) -> Result<JsValue, HostError> {
        self.incoming.recv().await.unwrap_or_else(|| {
            Err(HostError::new(
                HostErrorKind::Disconnected,
                "Sidecar pipe pumps stopped.",
            ))
        })
    }
    pub(super) async fn flush(&self) -> Result<(), HostError> {
        let (done, wait) = oneshot::channel();
        self.enqueue(WriteCommand::Flush(done))?;
        timeout(self.limits.io_timeout, wait)
            .await
            .map_err(|_| HostError::new(HostErrorKind::Timeout, "Sidecar writer flush timed out."))?
            .map_err(|_| {
                HostError::new(
                    HostErrorKind::Disconnected,
                    "Sidecar writer stopped before flush.",
                )
            })
    }
    pub(super) fn diagnostic(&self) -> String {
        let tail = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&tail.iter().copied().collect::<Vec<_>>())
            .trim()
            .into()
    }
    pub(super) fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    pub(super) async fn close(&mut self) -> Result<(), HostError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.outgoing.take();
        // Aborting the writer also closes a blocked stdin. Child exit, task joins,
        // and post-kill wait share finite bounds (TS stdio:123-139 leaves last wait unbounded).
        for pump in &self.pumps {
            pump.abort();
        }
        let grace = timeout(self.limits.close_timeout, self.child.wait()).await;
        let result = match grace {
            Ok(result) => result.map(|_| ()).map_err(HostError::io),
            Err(_) => {
                self.child.start_kill().map_err(HostError::io)?;
                timeout(self.limits.close_timeout, self.child.wait())
                    .await
                    .map_err(|_| {
                        HostError::new(HostErrorKind::Timeout, "Sidecar did not exit after kill.")
                    })?
                    .map(|_| ())
                    .map_err(HostError::io)
            }
        };
        let _ = timeout(self.limits.close_timeout, async {
            for task in self.pumps.drain(..) {
                let _ = task.await;
            }
        })
        .await;
        result
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        for pump in &self.pumps {
            pump.abort();
        }
        // Child::kill_on_drop covers dropped spawn/execute futures, too.
    }
}
async fn deliver(sender: &mpsc::Sender<Result<JsValue, HostError>>, bytes: &[u8]) -> bool {
    let value = match std::str::from_utf8(bytes) {
        Ok(text) if text.trim().is_empty() => return true,
        Ok(text) => parse_json(text)
            .map_err(|error| HostError::protocol(format!("Malformed NDJSON: {error}"))),
        Err(error) => Err(HostError::protocol(format!("Malformed UTF-8: {error}"))),
    };
    let valid = value.is_ok();
    sender.send(value).await.is_ok() && valid
}
