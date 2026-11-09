use super::*;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use std::task::{Context, Poll};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub struct ApiServerOptions {
    pub host: String,
    pub port: u16,
    pub api_key: String,
    pub model_name: String,
    pub cors_origins: Vec<String>,
    pub request_timeout: Duration,
}
impl Default for ApiServerOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8642,
            api_key: String::new(),
            model_name: "pilotdeck-gateway".into(),
            cors_origins: vec![],
            request_timeout: Duration::from_secs(300),
        }
    }
}
pub struct ApiServerChannel {
    options: ApiServerOptions,
    gateway: Arc<dyn ChannelGateway>,
    sessions: Sessions,
    active: Arc<Mutex<HashSet<String>>>,
    work: Arc<Work>,
}
impl ApiServerChannel {
    pub fn new(options: ApiServerOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Arc::new(Self {
            options,
            gateway,
            sessions: Default::default(),
            active: Default::default(),
            work: Default::default(),
        })
    }
    pub async fn start(self: &Arc<Self>) -> Result<HttpChannelHandle> {
        listen(
            &self.options.host,
            self.options.port,
            self.clone(),
            self.work.clone(),
        )
        .await
    }
    fn error(
        &self,
        status: u16,
        event: &str,
        code: &str,
        message: &str,
        hint: &str,
        scope: &str,
    ) -> Reply {
        json_response(
            status,
            http_error("api_server", status, event, code, message, hint, scope),
        )
    }
    fn submit_error(&self, message: &str) -> Value {
        http_error(
            "api_server",
            500,
            "channel_submit_failed",
            "channel_submit_failed",
            message,
            "PilotDeck failed before this API request could finish. Retry the request; if it repeats, check the API server and gateway logs.",
            "channel",
        )
    }
    fn cors(&self, response: &mut Reply, origin: &str) {
        if origin.is_empty() {
            return;
        }
        let wildcard = self.options.cors_origins.iter().any(|v| v == "*");
        if !wildcard && !self.options.cors_origins.iter().any(|v| v == origin) {
            return;
        }
        let headers = response.headers_mut();
        if let Ok(value) = hyper::header::HeaderValue::from_str(if wildcard { "*" } else { origin })
        {
            headers.insert("access-control-allow-origin", value);
        }
        headers.insert(
            "access-control-allow-methods",
            "GET, POST, OPTIONS, DELETE".parse().unwrap(),
        );
        headers.insert(
            "access-control-allow-headers",
            "Content-Type, Authorization, X-Hermes-Session-Id"
                .parse()
                .unwrap(),
        );
        headers.insert(
            "access-control-expose-headers",
            "X-Hermes-Session-Id".parse().unwrap(),
        );
        if !wildcard {
            headers.insert("vary", "Origin".parse().unwrap());
        }
    }
    async fn handle(self: &Arc<Self>, request: Request<Incoming>) -> Reply {
        if request.method() == "OPTIONS" {
            return response(204, "", "text/plain");
        }
        match request.uri().path() {
            "/health" => return json_response(200, json!({"status":"ok","platform":"api-server"})),
            "/v1/models" => {
                return json_response(
                    200,
                    json!({"object":"list","data":[{"id":self.options.model_name,"object":"model","created":now_ms()/1000,"owned_by":"gateway"}]}),
                );
            }
            "/v1/chat/completions" if request.method() == "POST" => {}
            _ => return response(404, "Not Found", "text/plain"),
        }
        if !self.options.api_key.is_empty()
            && !header(&request, "authorization")
                .strip_prefix("Bearer ")
                .is_some_and(|v| {
                    constant_equal(
                        crate::metadata::js_trim(v).as_bytes(),
                        self.options.api_key.as_bytes(),
                    )
                })
        {
            return self.error(
                401,
                "api_auth_failed",
                "invalid_api_key",
                "Invalid API key",
                "Check the API key and retry.",
                "http",
            );
        }
        let chat = crate::metadata::js_trim(header(&request, "x-hermes-session-id")).to_owned();
        let chat = if chat.is_empty() {
            format!("api-{}", uuid::Uuid::new_v4())
        } else {
            chat
        };
        let body = match read_body(request.into_body(), 1_000_000).await {
            Ok(body) => body,
            Err(error) => {
                return self.error(
                    413,
                    "api_request_too_large",
                    "request_too_large",
                    &format!("Request too large or unreadable: {error}"),
                    "Reduce the request size and retry.",
                    "http",
                );
            }
        };
        let body: Value = match serde_json::from_str(&body) {
            Ok(body) => body,
            Err(_) => {
                return self.error(
                    400,
                    "api_invalid_json",
                    "invalid_json",
                    "Invalid JSON",
                    "Send a valid JSON request body.",
                    "http",
                );
            }
        };
        let Some(messages) = body["messages"]
            .as_array()
            .filter(|messages| !messages.is_empty())
        else {
            return self.error(
                400,
                "api_messages_required",
                "messages_required",
                "messages is required",
                "Send at least one chat message.",
                "http",
            );
        };
        let input = normalize_content(&messages.last().unwrap()["content"]);
        if input.is_empty() {
            return self.empty_message();
        }
        let Some(active) = ActiveGuard::acquire(&self.active, &chat) else {
            return self.error(
                429,
                "session_busy",
                "session_busy",
                "Session already processing a request",
                "Wait for the current request to finish or start a new session.",
                "session",
            );
        };
        let (session, message, new) = self.sessions.resolve("api_server", &chat, &input);
        let streaming = body["stream"] == true;
        if new && message.is_empty() {
            drop(active);
            if streaming {
                return stream_response(
                    Full::new(Bytes::from(format!(
                        "{}{}",
                        chunk(&self.options.model_name, Some("已创建新会话。")),
                        done(&self.options.model_name)
                    )))
                    .boxed_unsync(),
                    None,
                );
            }
            return json_response(200, completion(&self.options.model_name, "已创建新会话。"));
        }
        if message.is_empty() {
            return self.empty_message();
        }
        let params = json!({"sessionKey":session,"channelKey":"api_server","message":message});
        if streaming {
            let (sender, receiver) = mpsc::unbounded_channel();
            let sender = Arc::new(Mutex::new(Some(sender)));
            let owner = self.clone();
            let output = sender.clone();
            self.work.spawn(async move {
                let _active = active;
                let sink = StreamSink { sender: output.clone(), model: owner.options.model_name.clone(), state: Default::default() };
                let call = owner.gateway.stream_turn(params, &sink);
                tokio::pin!(call);
                let timeout = tokio::time::sleep(owner.options.request_timeout);
                tokio::pin!(timeout);
                // TS closes the HTTP stream at its deadline and keeps the turn
                // running. The session remains active until that turn finishes.
                let result = tokio::select! { result = &mut call => result, _ = &mut timeout => { output.lock().unwrap().take(); call.await } };
                if let Some(sender) = output.lock().unwrap().take() {
                    let content = match result { Ok(()) => done(&owner.options.model_name), Err(error) => format!("data: {}\n\n",owner.submit_error(&error.message)) };
                    let _ = sender.send(Bytes::from(content));
                }
            });
            stream_response(ReceiverBody(receiver).boxed_unsync(), Some(&chat))
        } else {
            let _active = active;
            let sink = BufferedSink {
                state: Default::default(),
                webhook: false,
            };
            if let Err(error) = self.gateway.stream_turn(params, &sink).await {
                return json_response(500, self.submit_error(&error.message));
            }
            let text = sink.state.lock().unwrap().text.clone();
            let mut response = json_response(
                200,
                completion(&self.options.model_name, crate::metadata::js_trim(&text)),
            );
            session_header(&mut response, &chat);
            response
        }
    }
    fn empty_message(&self) -> Reply {
        self.error(
            400,
            "api_empty_message",
            "empty_message",
            "Empty message",
            "Send a non-empty user message.",
            "http",
        )
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use crate::config::routes::im_cron::{origin_channel_matches, session_field};
        if !origin_channel_matches(delivery, "api-server") {
            return Ok(false);
        }
        let _ = session_field(delivery);
        Ok(false)
    }
}
impl Service for ApiServerChannel {
    fn request(self: Arc<Self>, request: Request<Incoming>) -> RequestCall {
        Box::pin(async move {
            let origin = header(&request, "origin").to_owned();
            let mut response = self.handle(request).await;
            self.cors(&mut response, &origin);
            response
        })
    }
}
fn normalize_content(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(false) => String::new(),
        Value::String(text) => text.clone(),
        Value::Number(n) if n.as_f64() == Some(0.0) => String::new(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                if item.is_string() {
                    item.as_str().map(str::to_owned)
                } else if matches!(item["type"].as_str(), Some("text" | "input_text")) {
                    match &item["text"] {
                        Value::Null | Value::Bool(false) => None,
                        Value::Number(n) if n.as_f64() == Some(0.0) => None,
                        value => Some(js_string(value)),
                    }
                } else {
                    None
                }
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        other => js_string(other),
    }
}
pub(super) fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(text) => text.clone(),
        Value::Array(values) => values
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
        value => value.to_string(),
    }
}
fn completion(model: &str, text: &str) -> Value {
    json!({"id":format!("chatcmpl-{}",now_ms()),"object":"chat.completion","created":now_ms()/1000,"model":model,"choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}})
}
fn chunk(model: &str, text: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({"id":format!("chatcmpl-{}",now_ms()),"object":"chat.completion.chunk","created":now_ms()/1000,"model":model,"choices":[{"index":0,"delta":text.map(|text|json!({"content":text})).unwrap_or_else(||json!({})),"finish_reason":if text.is_none(){json!("stop")}else{Value::Null}}]})
    )
}
fn done(model: &str) -> String {
    format!("{}data: [DONE]\n\n", chunk(model, None))
}
fn session_header(response: &mut Reply, chat: &str) {
    if let Ok(value) = chat.parse() {
        response.headers_mut().insert("x-hermes-session-id", value);
    }
}
fn stream_response(body: Body, chat: Option<&str>) -> Reply {
    let mut response = Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap();
    if let Some(chat) = chat {
        session_header(&mut response, chat);
    }
    response
}
struct ReceiverBody(mpsc::UnboundedReceiver<Bytes>);
impl HttpBody for ReceiverBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, Infallible>>> {
        self.0
            .poll_recv(cx)
            .map(|item| item.map(|data| Ok(Frame::data(data))))
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}
struct StreamSink {
    sender: Arc<Mutex<Option<mpsc::UnboundedSender<Bytes>>>>,
    model: String,
    state: Mutex<RenderState>,
}
impl ChannelEventSink for StreamSink {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            if let Some(fragment) = self
                .state
                .lock()
                .unwrap()
                .event(&event, false)
                .filter(|s| !s.is_empty())
            {
                if let Some(sender) = self.sender.lock().unwrap().as_ref() {
                    let _ = sender.send(Bytes::from(chunk(&self.model, Some(&fragment))));
                }
            }
            Ok(())
        })
    }
}
