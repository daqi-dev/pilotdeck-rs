//! The SDK `Protocol` (request/response linking, timeouts, server requests)
//! over one connection, with the stdio link (`StdioClientTransport`). The
//! HTTP links live in [`super::http`].
//!
//! ## Frame order on stdio
//!
//! The TS transport handles one stdout chunk synchronously; what each
//! message causes is then ordered by microtask depth, which the probe
//! `_tmp/20261020-a65-pr10a/probes/order.mjs` measured against the real SDK:
//!
//! 1. depth 0 — a server request with no handler gets `Method not found`
//!    while the chunk is processed;
//! 2. depth 1 — the continuation of a request settled in the chunk:
//!    `Client.connect` sends `notifications/initialized`, or on a failed
//!    handshake closes the transport (stdin ends, later replies are dropped);
//! 3. depth 3 — the `ping` reply (`Promise.resolve().then().then().then`).
//!
//! Everything else a settled request causes runs many hops later, after the
//! chunk's replies. [`Wire`] reproduces the three depths explicitly: a
//! pending request carries a [`Settled`] hook that is applied after the
//! chunk's depth-0 frames and before its ping replies, all under the writer
//! queue's gate so a continuation woken in the chunk cannot jump ahead.
//!
//! Frames go through an unbounded queue to a writer task that owns stdin, so
//! no lock is held across a pipe write (a server blocked on a full stdout
//! pipe can never stall the reader).

use super::client::McpClientError;
use super::http::HttpLink;
use super::protocol::{Inbound, ResultSchema, parse_inbound, parse_result};
use crate::js_value;
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot, watch},
};

/// `DEFAULT_INHERITED_ENV_VARS` (non-Windows).
const DEFAULT_INHERITED_ENV_VARS: [&str; 6] = ["HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER"];
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// `TIMEOUT_MAX` of Node timers.
const NODE_TIMEOUT_MAX: f64 = 2_147_483_647.0;

pub(super) type Reply = Result<JsValue, McpClientError>;

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// A Node `setTimeout` delay: out-of-range or `NaN` becomes 1 ms.
pub(crate) fn node_delay(ms: f64) -> Duration {
    let ms = if (1.0..=NODE_TIMEOUT_MAX).contains(&ms) {
        ms
    } else {
        1.0
    };
    Duration::from_secs_f64(ms / 1000.0)
}

/// What the stdio transport does in the depth-1 turn after a pending request
/// settles (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Settled {
    /// The handshake continues: queue `notifications/initialized`.
    Initialized,
    /// The handshake failed: `void this.close()` ends stdin.
    End,
}

/// Decides [`Settled`] from the raw reply; `None` for ordinary requests.
pub(super) type SettledHook = fn(&Reply) -> Settled;

struct Pending {
    reply: oneshot::Sender<Reply>,
    hook: Option<SettledHook>,
}

#[derive(Default)]
struct WireState {
    /// `Protocol._transport` is set.
    connected: bool,
    next_id: i64,
    pending: HashMap<i64, Pending>,
}

struct StdioLink {
    /// `None` once the transport ended stdin or the process closed; dropping
    /// the sender lets the writer flush the queue and then end stdin. The
    /// mutex is also the per-chunk ordering gate.
    writer: Mutex<Option<mpsc::UnboundedSender<String>>>,
    pid: Option<u32>,
    exited: watch::Sender<bool>,
}

enum Link {
    Stdio(StdioLink),
    Http(Box<HttpLink>),
}

/// One connection: `Protocol` bookkeeping plus its transport.
pub(super) struct Wire {
    state: Mutex<WireState>,
    link: Link,
    /// `onclose` ran (process closed, or an HTTP transport was closed).
    closed: watch::Sender<bool>,
    /// `transport.close()` was called.
    closing: AtomicBool,
}

/// Depth-ordered output of one stdout chunk.
#[derive(Default)]
pub(super) struct Outbox {
    immediate: Vec<JsValue>,
    settled: Vec<Settled>,
    deferred: Vec<JsValue>,
}

pub(super) fn frame_text(value: &JsValue) -> Result<String, McpClientError> {
    Ok(js_value::json_stringify(value)
        .map_err(McpClientError::stack_overflow)?
        .unwrap_or_default())
}

pub(super) fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}

/// `Number(id)` for response routing.
fn message_id(id: &JsValue) -> Option<i64> {
    let number = match id {
        JsValue::Number(number) => *number,
        JsValue::String(text) => {
            js_value::string_to_number(&String::from_utf16_lossy(text.units()))
        }
        _ => return None,
    };
    (number.fract() == 0.0 && number.abs() < 9.0e15).then_some(number as i64)
}

fn initialized_frame() -> JsValue {
    object(vec![
        ("method", "notifications/initialized".into()),
        ("jsonrpc", "2.0".into()),
    ])
}

impl Wire {
    fn new(link: Link) -> Self {
        Self {
            state: Mutex::new(WireState {
                connected: true,
                ..WireState::default()
            }),
            link,
            closed: watch::channel(false).0,
            closing: AtomicBool::new(false),
        }
    }

    pub(super) fn http(link: HttpLink) -> Arc<Self> {
        Arc::new(Self::new(Link::Http(Box::new(link))))
    }

    pub(super) fn http_link(&self) -> Option<&HttpLink> {
        match &self.link {
            Link::Http(link) => Some(link),
            Link::Stdio(_) => None,
        }
    }

    pub(super) fn is_stdio(&self) -> bool {
        matches!(self.link, Link::Stdio(_))
    }

    /// `StdioClientTransport.start()`: spawn with the default inherited
    /// environment plus the spec env; stderr inherited.
    pub(super) fn spawn_stdio(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&str>,
    ) -> Result<Arc<Self>, McpClientError> {
        let mut process = Command::new(command);
        process
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for key in DEFAULT_INHERITED_ENV_VARS {
            if let Some(value) = std::env::var_os(key)
                && !value.as_encoded_bytes().starts_with(b"()")
            {
                process.env(key, value);
            }
        }
        for (key, value) in env {
            process.env(key, value);
        }
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        let mut child = process.spawn().map_err(|error| {
            let code = super::protocol::errno_text(&error).0;
            McpClientError::errno(&error, format!("spawn {command} {code}"))
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (sender, queue) = mpsc::unbounded_channel();
        tokio::spawn(write_queue(stdin, queue));
        let wire = Arc::new(Self::new(Link::Stdio(StdioLink {
            writer: Mutex::new(Some(sender)),
            pid: child.id(),
            exited: watch::channel(false).0,
        })));
        tokio::spawn(Self::pump(wire.clone(), child, stdout));
        Ok(wire)
    }

    fn stdio(&self) -> &StdioLink {
        match &self.link {
            Link::Stdio(link) => link,
            Link::Http(_) => unreachable!("stdio-only path"),
        }
    }

    /// Reads stdout and waits for exit; Node's `close` event is both.
    async fn pump(self: Arc<Self>, mut child: Child, stdout: Option<ChildStdout>) {
        let mut stdout = stdout;
        let mut buffer = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        let mut exited = false;
        while stdout.is_some() || !exited {
            tokio::select! {
                read = async { stdout.as_mut().expect("guarded").read(&mut chunk).await }, if stdout.is_some() => {
                    match read {
                        Ok(0) | Err(_) => stdout = None,
                        Ok(read) => {
                            buffer.extend_from_slice(&chunk[..read]);
                            self.process_chunk(&mut buffer);
                        }
                    }
                }
                _ = child.wait(), if !exited => {
                    exited = true;
                    self.stdio().exited.send_replace(true);
                }
            }
        }
        *lock(&self.stdio().writer) = None;
        self.on_close();
    }

    /// `Protocol._onclose`: the transport is gone and every pending request
    /// fails with `Connection closed`.
    pub(super) fn on_close(&self) {
        let pending = {
            let mut state = lock(&self.state);
            state.connected = false;
            std::mem::take(&mut state.pending)
        };
        for (_, entry) in pending {
            let _ = entry.reply.send(Err(McpClientError::connection_closed()));
        }
        self.closed.send_replace(true);
    }

    /// One stdout chunk: every complete line, then the depth-ordered output,
    /// all under the writer gate.
    pub(super) fn process_chunk(&self, buffer: &mut Vec<u8>) {
        let mut queue = lock(&self.stdio().writer);
        let mut outbox = Outbox::default();
        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            let text = text.strip_suffix('\r').unwrap_or(&text);
            if let Some(message) = parse_inbound(text) {
                self.dispatch(message, &mut outbox);
            }
        }
        let enqueue = |queue: &Option<mpsc::UnboundedSender<String>>, value: &JsValue| {
            if let (Some(queue), Ok(text)) = (queue.as_ref(), frame_text(value)) {
                let _ = queue.send(text + "\n");
            }
        };
        for value in &outbox.immediate {
            enqueue(&queue, value);
        }
        for settled in outbox.settled {
            match settled {
                Settled::Initialized => enqueue(&queue, &initialized_frame()),
                Settled::End => *queue = None,
            }
        }
        for value in &outbox.deferred {
            enqueue(&queue, value);
        }
    }

    /// `Protocol` message routing for one inbound message.
    pub(super) fn dispatch(&self, message: Inbound, outbox: &mut Outbox) {
        match message {
            Inbound::Result { id, result } => self.resolve(&id, Ok(result), outbox),
            Inbound::Error { id, code, message } => {
                let message = String::from_utf16_lossy(message.units());
                self.resolve(
                    &id.unwrap_or(JsValue::Undefined),
                    Err(McpClientError::rpc(code, &message)),
                    outbox,
                );
            }
            Inbound::Request { id, method } if method.equals_str("ping") => {
                outbox.deferred.push(object(vec![
                    ("result", JsValue::Object(JsObject::new())),
                    ("jsonrpc", "2.0".into()),
                    ("id", id),
                ]));
            }
            Inbound::Request { id, .. } => {
                outbox.immediate.push(object(vec![
                    ("jsonrpc", "2.0".into()),
                    ("id", id),
                    (
                        "error",
                        object(vec![
                            ("code", JsValue::Number(-32601.0)),
                            ("message", "Method not found".into()),
                        ]),
                    ),
                ]));
            }
            // Notifications (cancelled/progress/logging) have no client
            // effect here.
            Inbound::Notification => {}
        }
    }

    /// Dispatches messages that arrived over HTTP; replies to server
    /// requests are POSTed in the background, in depth order.
    pub(super) fn dispatch_http(self: &Arc<Self>, messages: Vec<Inbound>) {
        let mut outbox = Outbox::default();
        for message in messages {
            self.dispatch(message, &mut outbox);
        }
        for value in outbox.immediate.into_iter().chain(outbox.deferred) {
            self.send_detached(value);
        }
    }

    fn resolve(&self, id: &JsValue, reply: Reply, outbox: &mut Outbox) {
        let Some(id) = message_id(id) else {
            return;
        };
        let entry = lock(&self.state).pending.remove(&id);
        if let Some(entry) = entry {
            if let Some(hook) = entry.hook {
                outbox.settled.push(hook(&reply));
            }
            let _ = entry.reply.send(reply);
        }
    }

    fn fail(&self, id: i64, error: McpClientError) {
        let entry = lock(&self.state).pending.remove(&id);
        if let Some(entry) = entry {
            let _ = entry.reply.send(Err(error));
        }
    }

    /// The synchronous part of the stdio `send`: `stdin.write` queues the
    /// frame immediately in Node.
    fn send_stdio(link: &StdioLink, value: &JsValue) -> Result<(), McpClientError> {
        let text = frame_text(value)?;
        match lock(&link.writer).as_ref() {
            Some(queue) => {
                let _ = queue.send(text + "\n");
                Ok(())
            }
            None => Err(McpClientError::not_connected()),
        }
    }

    /// `transport.send(message)`.
    pub(super) async fn send(self: &Arc<Self>, value: JsValue) -> Result<(), McpClientError> {
        match &self.link {
            Link::Stdio(link) => Self::send_stdio(link, &value),
            Link::Http(link) => link.send(self, value).await,
        }
    }

    /// A send whose failure only reaches `onerror`. On stdio the frame is
    /// queued before this returns (a following close cannot overtake it).
    pub(super) fn send_detached(self: &Arc<Self>, value: JsValue) {
        match &self.link {
            Link::Stdio(link) => {
                let _ = Self::send_stdio(link, &value);
            }
            Link::Http(_) => {
                let wire = self.clone();
                tokio::spawn(async move {
                    let _ = wire.send(value).await;
                });
            }
        }
    }

    /// `Protocol.notification`.
    pub(super) async fn notification(self: &Arc<Self>, method: &str) -> Result<(), McpClientError> {
        if !lock(&self.state).connected {
            return Err(McpClientError::not_connected());
        }
        self.send(object(vec![
            ("method", method.into()),
            ("jsonrpc", "2.0".into()),
        ]))
        .await
    }

    /// `Protocol.request` with a timeout and result schema. A failed send
    /// rejects the request (the HTTP transports send in the background).
    pub(super) async fn request(
        self: &Arc<Self>,
        method: &str,
        params: Option<JsValue>,
        timeout_ms: f64,
        schema: ResultSchema,
        hook: Option<SettledHook>,
    ) -> Result<JsValue, McpClientError> {
        let (id, reply) = {
            let mut state = lock(&self.state);
            if !state.connected {
                return Err(McpClientError::not_connected());
            }
            let id = state.next_id;
            state.next_id += 1;
            let (sender, reply) = oneshot::channel();
            state.pending.insert(
                id,
                Pending {
                    reply: sender,
                    hook,
                },
            );
            (id, reply)
        };
        let mut entries = vec![("method", JsValue::String(method.into()))];
        if let Some(params) = params {
            entries.push(("params", params));
        }
        entries.push(("jsonrpc", "2.0".into()));
        entries.push(("id", JsValue::Number(id as f64)));
        let message = object(entries);
        match &self.link {
            Link::Stdio(_) => {
                if let Err(error) = self.send(message).await {
                    self.fail(id, error);
                }
            }
            Link::Http(_) => {
                let wire = self.clone();
                tokio::spawn(async move {
                    if let Err(error) = wire.send(message).await {
                        wire.fail(id, error);
                    }
                });
            }
        }
        match tokio::time::timeout(node_delay(timeout_ms), reply).await {
            Ok(Ok(Ok(raw))) => parse_result(schema, &raw)
                .map_err(|message| McpClientError::plain("ZodError", message)),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(McpClientError::connection_closed()),
            Err(_) => {
                lock(&self.state).pending.remove(&id);
                let error = McpClientError::rpc(-32001.0, "Request timed out");
                self.send_detached(object(vec![
                    ("jsonrpc", "2.0".into()),
                    ("method", "notifications/cancelled".into()),
                    (
                        "params",
                        object(vec![
                            ("requestId", JsValue::Number(id as f64)),
                            (
                                "reason",
                                JsValue::String(format!("McpError: {}", error.message()).into()),
                            ),
                        ]),
                    ),
                ]));
                Err(error)
            }
        }
    }

    /// `transport.close()`.
    pub(super) async fn close(&self) {
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        match &self.link {
            Link::Http(link) => {
                link.close();
                if !*self.closed.borrow() {
                    self.on_close();
                }
            }
            Link::Stdio(link) => {
                // `stdin.end()`, then SIGTERM after 2 s and SIGKILL after 2 s
                // more while the process has not exited.
                *lock(&link.writer) = None;
                let mut closed = self.closed.subscribe();
                for signal in [rustix::process::Signal::TERM, rustix::process::Signal::KILL] {
                    if *closed.borrow() {
                        return;
                    }
                    let _ =
                        tokio::time::timeout(CLOSE_GRACE, closed.wait_for(|closed| *closed)).await;
                    if *link.exited.borrow() {
                        return;
                    }
                    if let Some(pid) = link
                        .pid
                        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
                    {
                        let _ = rustix::process::kill_process(pid, signal);
                    }
                }
            }
        }
    }
}

/// Owns the child's stdin: writes queued frames in order, then ends stdin
/// (`stdin.end()`) once the queue's sender is dropped. After a failed write
/// the rest of the queue is discarded.
async fn write_queue(stdin: Option<ChildStdin>, mut queue: mpsc::UnboundedReceiver<String>) {
    let mut stdin = stdin;
    while let Some(line) = queue.recv().await {
        if let Some(pipe) = stdin.as_mut()
            && (pipe.write_all(line.as_bytes()).await.is_err() || pipe.flush().await.is_err())
        {
            stdin = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_delay_clamps_like_set_timeout() {
        assert_eq!(node_delay(f64::NAN), Duration::from_millis(1));
        assert_eq!(node_delay(0.0), Duration::from_millis(1));
        assert_eq!(node_delay(-5.0), Duration::from_millis(1));
        assert_eq!(node_delay(2_147_483_648.0), Duration::from_millis(1));
        assert_eq!(node_delay(300.0), Duration::from_millis(300));
    }

    #[test]
    fn message_ids_follow_number_coercion() {
        assert_eq!(message_id(&JsValue::Number(3.0)), Some(3));
        assert_eq!(message_id(&JsValue::String("0".into())), Some(0));
        assert_eq!(message_id(&JsValue::String(" 2 ".into())), Some(2));
        assert_eq!(message_id(&JsValue::String("x".into())), None);
        assert_eq!(message_id(&JsValue::Undefined), None);
    }

    /// A frame split across stdout reads is buffered until its newline, and
    /// a chunk's depth order holds: Method-not-found, then the settled hook,
    /// then ping replies.
    #[tokio::test]
    async fn chunks_buffer_partial_lines_and_order_by_depth() {
        let (sender, mut queue) = mpsc::unbounded_channel();
        let wire = Wire::new(Link::Stdio(StdioLink {
            writer: Mutex::new(Some(sender)),
            pid: None,
            exited: watch::channel(false).0,
        }));
        let (reply, _receiver) = oneshot::channel();
        lock(&wire.state).pending.insert(
            0,
            Pending {
                reply,
                hook: Some(|_| Settled::Initialized),
            },
        );
        let mut buffer = br#"{"jsonrpc":"2.0","id":9,"method":"ping"}
{"jsonrpc":"2.0","id":0,"res"#
            .to_vec();
        wire.process_chunk(&mut buffer);
        buffer.extend_from_slice(
            b"ult\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"roots/list\"}\n",
        );
        wire.process_chunk(&mut buffer);
        assert!(buffer.is_empty());
        let mut frames = Vec::new();
        while let Ok(frame) = queue.try_recv() {
            frames.push(frame);
        }
        assert_eq!(
            frames,
            [
                "{\"result\":{},\"jsonrpc\":\"2.0\",\"id\":9}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":8,\"error\":{\"code\":-32601,\"message\":\"Method not found\"}}\n",
                "{\"method\":\"notifications/initialized\",\"jsonrpc\":\"2.0\"}\n",
            ]
        );
    }
}
