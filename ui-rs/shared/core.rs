//! Probe-only shared protocol, typed island bridge and DOM input compatibility.
use gloo_net::http::Request;
use gloo_timers::callback::{Interval, Timeout};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, prelude::*};
use web_sys::{Event, HtmlInputElement, HtmlTextAreaElement, MessageEvent, WebSocket};

pub const DRAFT_KEY: &str = "draft_input_probe:s";

#[wasm_bindgen(module = "/../shared/island.js")]
extern "C" {
    #[derive(PartialEq)]
    pub type TerminalIsland;
    #[wasm_bindgen(constructor)]
    pub fn new() -> TerminalIsland;
    #[wasm_bindgen(method)]
    pub fn mount(this: &TerminalIsland, id: &str);
    #[wasm_bindgen(method)]
    pub fn detach(this: &TerminalIsland);
    #[wasm_bindgen(method)]
    pub fn focus(this: &TerminalIsland);
    #[wasm_bindgen(method)]
    pub fn write(this: &TerminalIsland, data: &str);
    #[wasm_bindgen(method)]
    pub fn connect(this: &TerminalIsland, url: &str);
    #[wasm_bindgen(method)]
    pub fn snapshot(this: &TerminalIsland) -> String;
}

/// Exported typed facade: the Rust shell owns lifecycle, the JS owner keeps xterm DOM.
#[wasm_bindgen]
#[derive(PartialEq)]
pub struct TerminalBridge {
    inner: TerminalIsland,
}
#[wasm_bindgen]
impl TerminalBridge {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: TerminalIsland::new(),
        }
    }
    pub fn mount(&self, id: &str) {
        self.inner.mount(id);
    }
    pub fn detach(&self) {
        self.inner.detach();
    }
    pub fn focus(&self) {
        self.inner.focus();
    }
    pub fn write(&self, data: &str) {
        self.inner.write(data);
    }
    pub fn connect(&self, url: &str) {
        self.inner.connect(url);
    }
    pub fn snapshot(&self) -> String {
        self.inner.snapshot()
    }
}
impl Default for TerminalBridge {
    fn default() -> Self {
        Self::new()
    }
}

pub fn window() -> web_sys::Window {
    web_sys::window().expect("browser window")
}
pub fn get(key: &str) -> String {
    window()
        .local_storage()
        .unwrap()
        .unwrap()
        .get_item(key)
        .unwrap()
        .unwrap_or_default()
}
pub fn set(key: &str, value: &str) {
    window()
        .local_storage()
        .unwrap()
        .unwrap()
        .set_item(key, value)
        .unwrap();
}
pub fn input(id: &str) -> String {
    window()
        .document()
        .unwrap()
        .get_element_by_id(id)
        .unwrap()
        .unchecked_into::<HtmlInputElement>()
        .value()
}
pub fn ws_url(path: &str) -> String {
    let loc = window().location();
    let protocol = if loc.protocol().unwrap() == "https:" {
        "wss"
    } else {
        "ws"
    };
    format!(
        "{protocol}://{}{}?token={}",
        loc.host().unwrap(),
        path,
        js_sys::encode_uri_component(&get("auth-token"))
    )
}

#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub seq: u32,
    #[serde(default)]
    pub text: String,
}
#[derive(Clone, Default, Debug)]
pub struct Stream {
    pub received: u32,
    pub missing: u32,
    pub text: String,
}
impl Stream {
    pub fn accept(&mut self, f: &Frame) {
        if f.seq != self.received + 1 {
            self.missing += 1;
        }
        self.received += 1;
        self.text.push_str(&f.text);
    }
}

pub async fn login() -> Result<String, String> {
    let body = serde_json::json!({"username":input("username"), "password":input("password")});
    let response = Request::post("/api/auth/login")
        .json(&body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(format!("login HTTP {}", response.status()));
    }
    let data: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let token = data["token"].as_str().ok_or("missing token")?.to_owned();
    set("auth-token", &token);
    Ok(token)
}
pub async fn fetch(path: &str) -> Result<serde_json::Value, String> {
    let response = Request::get(path)
        .header("Authorization", &format!("Bearer {}", get("auth-token")))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.ok() {
        return Err(format!("{path}: HTTP {}", response.status()));
    }
    if let Some(token) = response.headers().get("X-Refreshed-Token") {
        set("auth-token", &token);
    }
    response.json().await.map_err(|e| e.to_string())
}
pub async fn lists() -> Result<String, String> {
    let projects = fetch("/api/projects").await?;
    // Source api.js:231: project-scoped /api/projects/:name/sessions, not /api/sessions.
    let sessions = fetch("/api/projects/probe/sessions?limit=5&offset=0").await?;
    Ok(format!("Projects: {projects}\nSessions: {sessions}"))
}

pub fn composer() -> HtmlTextAreaElement {
    window()
        .document()
        .unwrap()
        .get_element_by_id("composer")
        .unwrap()
        .unchecked_into()
}
pub fn restore_draft() {
    composer().set_value(&get(DRAFT_KEY));
}
pub fn save_draft() {
    set(DRAFT_KEY, &composer().value());
}
pub fn clear_draft() {
    composer().set_value("");
    set(DRAFT_KEY, "");
}

struct SocketBindings {
    socket: WebSocket,
    _open: Closure<dyn FnMut(Event)>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut(Event)>,
}
impl Drop for SocketBindings {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }
}
struct Connection {
    socket: Option<SocketBindings>,
    retry: Option<Timeout>,
    ping: Option<Interval>,
    attempt: u32,
    callback: Rc<RefCell<dyn FnMut(Frame)>>,
}
pub struct MainSocket(Rc<RefCell<Connection>>);
impl MainSocket {
    pub fn new(callback: impl FnMut(Frame) + 'static) -> Self {
        let inner = Rc::new(RefCell::new(Connection {
            socket: None,
            retry: None,
            ping: None,
            attempt: 0,
            callback: Rc::new(RefCell::new(callback)),
        }));
        Self::connect(&inner);
        Self(inner)
    }
    fn connect(inner: &Rc<RefCell<Connection>>) {
        let socket = WebSocket::new(&ws_url("/ws")).unwrap();
        let state = Rc::downgrade(inner);
        let opened_socket = socket.clone();
        let open = Closure::<dyn FnMut(Event)>::new(move |_| {
            let Some(state) = state.upgrade() else { return };
            opened_socket
                .send_with_str(r#"{"type":"watch-session","sessionId":"s"}"#)
                .unwrap();
            let ping_socket = opened_socket.clone();
            let ping = Interval::new(30_000, move || {
                let _ = ping_socket.send_with_str(r#"{"type":"ping"}"#);
            });
            let mut state = state.borrow_mut();
            state.attempt = 0;
            state.ping = Some(ping);
        });
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));

        let callback = inner.borrow().callback.clone();
        let message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Some(text) = e.data().as_string()
                && let Ok(frame) = serde_json::from_str::<Frame>(&text)
            {
                callback.borrow_mut()(frame);
            }
        });
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));

        let state = Rc::downgrade(inner);
        let close = Closure::<dyn FnMut(Event)>::new(move |_| {
            let Some(state) = state.upgrade() else { return };
            let mut conn = state.borrow_mut();
            conn.ping = None;
            conn.attempt += 1;
            let delay = reconnect_delay(conn.attempt);
            let next = Rc::downgrade(&state);
            conn.retry = Some(Timeout::new(delay, move || {
                if let Some(state) = next.upgrade() {
                    Self::connect(&state);
                }
            }));
        });
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        inner.borrow_mut().socket = Some(SocketBindings {
            socket,
            _open: open,
            _message: message,
            _close: close,
        });
    }
}
impl Drop for MainSocket {
    fn drop(&mut self) {
        let mut conn = self.0.borrow_mut();
        conn.retry = None;
        conn.ping = None;
        conn.socket = None;
    }
}
pub fn reconnect_delay(attempt: u32) -> u32 {
    1_000u32
        .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(30_000)
}
