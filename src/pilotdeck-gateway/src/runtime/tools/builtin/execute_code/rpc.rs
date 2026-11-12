//! Authenticated loopback helpers. The enclosing tool future owns every client.
use super::*;
use crate::host::modules::capability::ToolCall;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
const MAX_LINE: usize = 1024 * 1024;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const MAX_CLIENTS: usize = 8;
pub(super) struct State {
    pub hook: crate::runtime::tools::NestedToolExecutor,
    pub token: Option<String>,
    pub allowed: Vec<&'static str>,
    pub outer: JsString,
    pub max: usize,
    pub made: AtomicUsize,
    pub log: std::sync::Mutex<Vec<JsValue>>,
    pub context: ToolRunContext,
}
impl State {
    pub fn new(
        hook: crate::runtime::tools::NestedToolExecutor,
        token: Option<String>,
        allowed: Vec<&'static str>,
        outer: JsString,
        max: usize,
        context: ToolRunContext,
    ) -> Self {
        Self {
            hook,
            token,
            allowed,
            outer,
            max,
            made: AtomicUsize::new(0),
            log: std::sync::Mutex::new(Vec::new()),
            context,
        }
    }
    pub async fn handle(&self, line: &str) -> Result<JsValue, ToolError> {
        let request = match pilotdeck_protocol::parse_json(line) {
            Ok(value) => value,
            Err(_) => {
                let detail = pilotdeck_protocol::json_syntax::parse_error(line)
                    .unwrap_or_else(|| "Invalid JSON.".into());
                return Ok(error(
                    "invalid_rpc",
                    format!("Invalid RPC request: {detail}"),
                ));
            }
        };
        let value = |key| request.get(key).cloned().unwrap_or(JsValue::Undefined);
        if let Some(token) = &self.token
            && value("token").as_string().map(text).as_deref() != Some(token.as_str())
        {
            return Ok(error(
                "invalid_rpc_token",
                "Invalid execute_code RPC token.",
            ));
        }
        let name = value("tool").as_string().cloned().unwrap_or_default();
        if !self.allowed.iter().any(|item| name.equals_str(item)) {
            return Ok(error(
                "tool_not_allowed",
                format!("Tool '{}' is not available in execute_code.", text(&name)),
            ));
        }
        let mut current = self.made.load(Ordering::SeqCst);
        let sequence = loop {
            if current >= self.max {
                return Ok(error(
                    "tool_call_limit_reached",
                    format!(
                        "Tool call limit reached ({}). No more tool calls allowed in this execution.",
                        self.max
                    ),
                ));
            }
            match self.made.compare_exchange_weak(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(n) => break n + 1,
                Err(n) => current = n,
            }
        };
        let args = JsValue::Object(value("args").as_object().cloned().unwrap_or_default());
        let mut id = self.outer.units().to_vec();
        id.extend(JsString::from(format!(":code:{sequence}")).units());
        let started = self
            .context
            .now_ms()
            .map_err(|e| ToolError::failed(e.to_string()))?;
        let result = (self.hook)(ToolCall {
            id: JsString::from_utf16(id),
            name: name.clone(),
            input: args,
        })
        .await
        .map_err(|e| ToolError::failed(e.to_string()))?;
        let ended = self
            .context
            .now_ms()
            .map_err(|e| ToolError::failed(e.to_string()))?;
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(JsValue::Object(object([
                ("tool", string(name)),
                ("duration_ms", JsValue::Number(ended - started)),
                (
                    "ok",
                    JsValue::Bool(result.get("type") == Some(&string("success"))),
                ),
            ])));
        if !response_fits(&result) {
            return Ok(error(
                "rpc_output_limit",
                "execute_code RPC response exceeded 2MiB.",
            ));
        }
        Ok(to_response(&result))
    }
}
// Bound the work/allocation before cloning, concatenating or JSON encoding a
// hostile helper result. Six bytes per UTF-16 unit is a safe escape bound;
// allowing 6x the wire cap keeps ordinary <=2MiB ASCII responses admissible.
// Actual encoded bytes are still checked against 2MiB in client().
fn response_fits(result: &JsObject) -> bool {
    fn charge(budget: &mut usize, n: usize) -> bool {
        match budget.checked_sub(n) {
            Some(left) => {
                *budget = left;
                true
            }
            None => false,
        }
    }
    fn value(input: &JsValue, budget: &mut usize, depth: usize) -> bool {
        if depth > 64 || !charge(budget, 4) {
            return false;
        }
        match input {
            JsValue::String(s) => charge(budget, s.len_utf16().saturating_mul(6)),
            JsValue::Number(_) => charge(budget, 32),
            JsValue::Array(items) => items.iter().all(|item| value(item, budget, depth + 1)),
            JsValue::Object(object) => object.entries().iter().all(|(key, item)| {
                charge(budget, key.len_utf16().saturating_mul(6).saturating_add(4))
                    && value(item, budget, depth + 1)
            }),
            _ => true,
        }
    }
    let mut budget = MAX_RESPONSE * 6;
    result.entries().iter().all(|(key, item)| {
        charge(
            &mut budget,
            key.len_utf16().saturating_mul(6).saturating_add(4),
        ) && value(item, &mut budget, 1)
    })
}
pub(super) fn error(code: &str, message: impl Into<JsString>) -> JsValue {
    JsValue::Object(object([("error", string(message)), ("code", string(code))]))
}
fn content_text(block: &JsValue) -> String {
    let value = |key| block.get(key).cloned().unwrap_or(JsValue::Undefined);
    let display = |key| {
        crate::js_value::to_string_units_checked(&value(key))
            .map(JsString::from_utf16)
            .map(|v| text(&v))
            .unwrap_or_default()
    };
    let len = value("data")
        .as_string()
        .map(|v| v.len_utf16())
        .unwrap_or(0);
    match value("type").as_string().map(text).as_deref() {
        Some("text") => value("text").as_string().map(text).unwrap_or_default(),
        Some("json") => pilotdeck_protocol::stringify_json(&value("value"))
            .ok()
            .flatten()
            .unwrap_or_default(),
        Some("image") => format!("[Image: {}, {len} base64 characters]", display("mimeType")),
        Some("pdf") => format!(
            "[PDF: {}, {len} base64 characters{}]",
            display("mimeType"),
            if super::super::truthy(&value("pages")) {
                format!(", {} pages", display("pages"))
            } else {
                String::new()
            }
        ),
        Some("file") => format!(
            "[File: {}{}{}]",
            display("path"),
            if super::super::truthy(&value("mimeType")) {
                format!(", {}", display("mimeType"))
            } else {
                String::new()
            },
            if super::super::truthy(&value("description")) {
                format!(", {}", display("description"))
            } else {
                String::new()
            }
        ),
        _ => String::new(),
    }
}
pub(super) fn to_response(result: &JsObject) -> JsValue {
    let content = match result.get("content") {
        Some(JsValue::Array(blocks)) => blocks
            .iter()
            .map(content_text)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let mut out = JsObject::new();
    if result.get("type") == Some(&string("error")) {
        static UNDEFINED: JsValue = JsValue::Undefined;
        let failure = result.get("error").unwrap_or(&UNDEFINED);
        let mut message = failure
            .get("message")
            .and_then(JsValue::as_string)
            .map(text)
            .unwrap_or_default();
        if let Some(JsValue::Array(issues)) = failure.get("details").and_then(|v| v.get("issues")) {
            for issue in issues {
                if let Some(v) = issue
                    .get("message")
                    .and_then(JsValue::as_string)
                    .filter(|s| !s.is_empty())
                {
                    message.push('\n');
                    message.push_str(&text(v));
                }
            }
        }
        out.insert("error".into(), string(message));
        if let Some(code) = failure.get("code") {
            out.insert("code".into(), code.clone());
        }
        out.insert("content".into(), string(content));
    } else {
        out.insert("content".into(), string(content));
        if let Some(data) = result.get("data") {
            out.insert("data".into(), data.clone());
        }
    }
    if let Some(metadata) = result.get("metadata") {
        out.insert("metadata".into(), metadata.clone());
    }
    JsValue::Object(out)
}
async fn read_line(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Incomplete execute_code RPC line.",
                ))
            };
        }
        let take = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |n| n + 1);
        if take > MAX_LINE - line.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "execute_code RPC request exceeded 1MiB.",
            ));
        }
        let done = available[take - 1] == b'\n';
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if done {
            return Ok(Some(line));
        }
    }
}
async fn client(stream: TcpStream, state: Arc<State>) -> Result<(), ToolError> {
    let mut reader = BufReader::new(stream);
    while let Some(line) = read_line(&mut reader)
        .await
        .map_err(|e| ToolError::failed(e.to_string()))?
    {
        let line = String::from_utf8(line)
            .map_err(|_| ToolError::failed("execute_code RPC is not valid UTF-8."))?;
        if line.trim().is_empty() {
            continue;
        }
        let response = state.handle(line.trim()).await?;
        let mut bytes = pilotdeck_protocol::stringify_json(&response)
            .map_err(|e| ToolError::failed(e.to_string()))?
            .unwrap_or_else(|| "{}".into())
            .into_bytes();
        if bytes.len() > MAX_RESPONSE {
            bytes=br#"{"error":"execute_code RPC response exceeded 2MiB.","code":"rpc_output_limit"}"#.to_vec();
        }
        bytes.push(b'\n');
        reader
            .get_mut()
            .write_all(&bytes)
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?;
    }
    Ok(())
}
pub(super) struct Server {
    listener: Option<TcpListener>,
    state: Arc<State>,
    clients: JoinSet<Result<(), ToolError>>,
}
impl Server {
    pub fn new(listener: TcpListener, state: Arc<State>) -> Self {
        Self {
            listener: Some(listener),
            state,
            clients: JoinSet::new(),
        }
    }
    pub async fn serve(&mut self) -> Result<(), ToolError> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| ToolError::failed("execute_code RPC listener is closed."))?;
        loop {
            tokio::select! {
                result=listener.accept(),if self.clients.len()<MAX_CLIENTS=>{
                    let (stream,peer)=result.map_err(|e|ToolError::failed(e.to_string()))?;
                    if !peer.ip().is_loopback() {return Err(ToolError::failed("Non-loopback execute_code RPC client."));}
                    let state=self.state.clone();self.clients.spawn(async move {client(stream,state).await});
                },
                result=self.clients.join_next(),if !self.clients.is_empty()=>{
                    match result {
                        Some(Ok(Ok(())))=>{},
                        Some(Ok(Err(error)))=>return Err(error),
                        Some(Err(error))=>return Err(ToolError::failed(error.to_string())),
                        None=>{},
                    }
                }
            }
        }
    }
    pub async fn shutdown(&mut self) -> Result<(), ToolError> {
        self.clients.abort_all();
        let drain = async { while self.clients.join_next().await.is_some() {} };
        let drained = tokio::time::timeout(std::time::Duration::from_secs(2), drain)
            .await
            .map_err(|_| {
                ToolError::failed("execute_code nested tool cancellation could not be confirmed.")
            });
        let listener_shutdown = if let Some(listener) = self.listener.take() {
            // Preserve client cancellation/drain ordering, but release the
            // listening socket before the caller starts workspace cleanup.
            #[cfg(target_os = "linux")]
            let result = socket2::SockRef::from(&listener).shutdown(std::net::Shutdown::Both);
            #[cfg(not(target_os = "linux"))]
            let result: std::io::Result<()> = Ok(());
            drop(listener);
            result.map_err(|error| {
                ToolError::failed(format!("execute_code RPC listener shutdown failed: {error}"))
            })
        } else {
            Ok(())
        };
        // A client-drain failure must not leave the listener open.
        drained?;
        listener_shutdown
    }
}
#[cfg(test)]
mod bounds_tests {
    use super::*;
    #[test]
    fn rejects_huge_or_deep_results_before_json_and_preserves_small_response() {
        assert!(response_fits(&object([("data", string("small"))])));
        assert!(!response_fits(&object([(
            "data",
            string("x".repeat(MAX_RESPONSE + 1))
        )])));
        let mut value = JsValue::Null;
        for _ in 0..70 {
            value = JsValue::Array(vec![value]);
        }
        assert!(!response_fits(&object([("data", value)])));
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn shutdown_closes_listener_with_a_duplicate_descriptor() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let duplicate = socket2::SockRef::from(&listener)
            .try_clone()
            .expect("retain the same listening socket across shutdown");
        let root = super::super::Workspace::create().unwrap();
        let state = Arc::new(State::new(
            Arc::new(|_| Box::pin(async { unreachable!() })),
            Some("secret".into()),
            vec![],
            "outer".into(),
            50,
            super::super::tests::make_context(
                &root.path,
                crate::runtime::network::Cancellation::new(),
            ),
        ));
        let mut server = Server::new(listener, state);
        let mut peer = BufReader::new(TcpStream::connect(address).await.unwrap());
        {
            let serving = server.serve();
            tokio::pin!(serving);
            let response = async {
                peer.get_mut().write_all(b"{}\n").await.unwrap();
                let mut line = String::new();
                peer.read_line(&mut line).await.unwrap();
                let value: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(value["code"], "invalid_rpc_token");
            };
            tokio::select! {
                result = &mut serving => panic!("RPC serving stopped before the response: {result:?}"),
                result = tokio::time::timeout(std::time::Duration::from_secs(60), response) => {
                    result.expect("the real accepted RPC client receives its response");
                }
            }
        }
        server.shutdown().await.expect("shutdown completes");
        assert!(server.clients.is_empty(), "the accepted client was joined");
        assert!(
            TcpStream::connect(address).await.is_err(),
            "shutdown must stop accepting even while a duplicate descriptor survives"
        );
        let successor = TcpListener::bind(address)
            .await
            .expect("shutdown permits rebinding while the duplicate survives");
        assert!(duplicate.local_addr().is_ok(), "the duplicate remains owned");
        server.shutdown().await.expect("repeated shutdown is harmless");
        assert!(
            server.serve().await.is_err(),
            "a stopped server cannot accept again"
        );
        drop(peer);
        drop(successor);
        drop(duplicate);
    }

    #[tokio::test]
    async fn shutdown_waits_for_owned_client_future_to_drop() {
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let root = super::super::Workspace::create().unwrap();
        let state = Arc::new(State::new(
            Arc::new(|_| Box::pin(async { unreachable!() })),
            Some("secret".into()),
            vec![],
            "outer".into(),
            50,
            super::super::tests::make_context(
                &root.path,
                crate::runtime::network::Cancellation::new(),
            ),
        ));
        let mut server = Server::new(listener, state);
        let owned = Dropped(dropped.clone());
        server.clients.spawn(async move {
            let _owned = owned;
            std::future::pending::<()>().await;
            Ok(())
        });
        server.shutdown().await.unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(server.clients.is_empty());
    }
}
