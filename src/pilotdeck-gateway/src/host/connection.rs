//! One owned child and independent pipe pumps. No callback runs in a pipe pump.
use super::{HostError, HostErrorKind, Limits};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
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
    Ack(String),
    Flush(oneshot::Sender<()>),
}

pub(super) struct Connection {
    child: Child,
    outgoing: Option<mpsc::Sender<WriteCommand>>,
    incoming: mpsc::Receiver<Result<JsValue, HostError>>,
    pumps: Vec<JoinHandle<()>>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    pending_required: Arc<AtomicUsize>,
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
        let pending_required = Arc::new(AtomicUsize::new(0));
        let pending = pending_required.clone();
        let writer = tokio::spawn(async move {
            while let Some(command) = writes.recv().await {
                let result = match command {
                    WriteCommand::Frame(line) => {
                        let result = stdin.write_all(line.as_bytes()).await;
                        if result.is_ok() {
                            pending.fetch_sub(1, Ordering::SeqCst);
                        }
                        result
                    }
                    WriteCommand::Ack(line) => {
                        // TS client:961-970: ACK failures never enter the input stream.
                        if let Err(error) = stdin.write_all(line.as_bytes()).await {
                            best_effort_log(format_args!(
                                "Sidecar ACK write failed (best-effort): {error}"
                            ));
                        }
                        // Keep draining: a later required frame must report its own failure.
                        continue;
                    }
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
            pending_required,
            limits,
            closed: false,
        })
    }

    pub(super) fn send(&self, value: &JsValue) -> Result<(), HostError> {
        let line = self.encode(value)?;
        // Increment before enqueue: the writer may run immediately on another worker.
        self.pending_required.fetch_add(1, Ordering::SeqCst);
        let result = self.enqueue(WriteCommand::Frame(line));
        if result.is_err() {
            self.pending_required.fetch_sub(1, Ordering::SeqCst);
        }
        result
    }
    pub(super) fn send_ack(&self, value: &JsValue) {
        if let Err(error) = self
            .encode(value)
            .and_then(|line| self.enqueue(WriteCommand::Ack(line)))
        {
            best_effort_log(format_args!(
                "Sidecar ACK enqueue failed (best-effort): {error}"
            ));
        }
    }
    fn encode(&self, value: &JsValue) -> Result<String, HostError> {
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
        Ok(line)
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
    pub(super) async fn finish_terminal(&self) -> Result<(), HostError> {
        if let Err(error) = self.flush().await {
            // A pending module response (or other required frame) still fails closed.
            // Once those writes succeeded, only best-effort ACKs/flush remain.
            if self.pending_required.load(Ordering::SeqCst) != 0 {
                return Err(error);
            }
            best_effort_log(format_args!(
                "Sidecar terminal ACK flush failed (best-effort): {error}"
            ));
        } else if self.pending_required.load(Ordering::SeqCst) != 0 {
            // Callers are serial today; this guards a required frame enqueued behind the Flush.
            return Err(HostError::new(
                HostErrorKind::Disconnected,
                "Sidecar required frame was not written before terminal flush.",
            ));
        }
        Ok(())
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

/// Diagnostics on best-effort paths must never fail the turn: `eprintln!` panics
/// when the host's stderr is closed, so write errors are ignored here.
fn best_effort_log(message: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostClient;
    use std::time::Duration;

    fn peer(mode: &str) -> Command {
        let mut command = Command::new("python3");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/a6-host/fixture.py"
            ))
            .arg(mode);
        command
    }
    fn limits() -> Limits {
        Limits {
            io_timeout: Duration::from_millis(20),
            close_timeout: Duration::from_millis(50),
            ..Limits::default()
        }
    }
    fn controlled_queue(capacity: usize) -> (Connection, mpsc::Receiver<WriteCommand>) {
        let mut connection = Connection::spawn(&mut peer("handshake-hang"), limits()).unwrap();
        let (sender, receiver) = mpsc::channel(capacity);
        // Freeze only the writer; the private channel makes pressure deterministic.
        connection.pumps[1].abort();
        connection.outgoing = Some(sender);
        (connection, receiver)
    }
    #[tokio::test]
    async fn ack_queue_full_and_closed_are_best_effort_but_required_frames_fail() {
        let (mut connection, receiver) = controlled_queue(1);
        let frame = parse_json(r#"{"kind":"request","method":"ack"}"#).unwrap();
        connection.send_ack(&frame); // Fill the only slot.
        connection.send_ack(&frame); // Full: must not fail execution.
        let error = connection.send(&frame).unwrap_err();
        assert!(error.message.contains("queue full"), "{error}");
        assert_eq!(connection.pending_required.load(Ordering::SeqCst), 0);
        connection.finish_terminal().await.unwrap();
        drop(receiver);
        connection.send_ack(&frame); // Closed: must not fail execution.
        let error = connection.send(&frame).unwrap_err();
        assert!(error.message.contains("channel closed"), "{error}");
        assert_eq!(connection.pending_required.load(Ordering::SeqCst), 0);
        connection.finish_terminal().await.unwrap();
        connection.close().await.unwrap();
    }
    #[tokio::test]
    async fn terminal_flush_timeout_is_diagnostic_unless_required_frames_are_pending() {
        let (mut connection, _receiver) = controlled_queue(4);
        // The receiver stays alive without consuming the flush command.
        connection.finish_terminal().await.unwrap();
        let response = parse_json(r#"{"kind":"response","ok":true}"#).unwrap();
        connection.send(&response).unwrap();
        let error = connection.finish_terminal().await.unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Timeout);
        assert!(error.message.contains("writer flush timed out"), "{error}");
        assert_eq!(connection.pending_required.load(Ordering::SeqCst), 1);
        connection.close().await.unwrap();
    }
    #[tokio::test]
    async fn terminal_flush_success_still_fails_when_required_frame_is_pending() {
        let (mut connection, mut receiver) = controlled_queue(4);
        let drain = tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                if let WriteCommand::Flush(done) = command {
                    let _ = done.send(());
                }
            }
        });
        connection.finish_terminal().await.unwrap();
        // A required frame counted but not yet written when the Flush completes.
        connection.pending_required.fetch_add(1, Ordering::SeqCst);
        let error = connection.finish_terminal().await.unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Disconnected);
        assert!(
            error.message.contains("required frame was not written"),
            "{error}"
        );
        connection.pending_required.fetch_sub(1, Ordering::SeqCst);
        connection.close().await.unwrap();
        drain.abort();
    }
    #[tokio::test]
    async fn ack_pipe_failure_keeps_writer_alive_to_report_required_write_failure() {
        let mut client = HostClient::spawn(&mut peer("terminal-then-exit"), None, limits())
            .await
            .unwrap();
        let connection = &mut client.connection;
        connection.send(&parse_json(r#"{"kind":"request","messageId":"execute","method":"execute","runId":"run","operationId":"operation","requestId":"request","sessionId":"session","turnId":"turn","payload":{}}"#).unwrap()).unwrap();
        let accepted = connection.receive().await.unwrap();
        assert_eq!(accepted.get("ok").and_then(JsValue::as_bool), Some(true));
        // The fixture closes stdin before writing this event, so ACK must hit EPIPE.
        let event = connection.receive().await.unwrap();
        assert_eq!(event.get("final").and_then(JsValue::as_bool), Some(false));
        let ack = parse_json(r#"{"kind":"request","messageId":"ack-0","method":"ack","streamId":"stream","lastAppliedSequence":0}"#).unwrap();
        connection.send_ack(&ack);
        connection
            .flush()
            .await
            .expect("writer must continue after ACK EPIPE");
        let final_event = connection.receive().await.unwrap();
        assert_eq!(
            final_event.get("final").and_then(JsValue::as_bool),
            Some(true)
        );
        let eof = connection.receive().await.unwrap_err();
        assert_eq!(eof.kind, HostErrorKind::Disconnected);
        assert!(matches!(
            connection.incoming.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let response = parse_json(r#"{"kind":"response","inReplyTo":"call","ok":true}"#).unwrap();
        connection.send(&response).unwrap();
        let error = timeout(Duration::from_secs(2), connection.receive())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Io);
        assert_eq!(connection.pending_required.load(Ordering::SeqCst), 1);
        assert!(connection.finish_terminal().await.is_err());
        connection.close().await.unwrap();
    }
}
