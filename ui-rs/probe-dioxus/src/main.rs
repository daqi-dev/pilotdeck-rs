use dioxus::prelude::*;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
#[path = "../../shared/core.rs"]
mod core;

#[component]
fn TerminalHost(island: Rc<core::TerminalBridge>) -> Element {
    let cleanup = island.clone();
    use_drop(move || cleanup.detach());
    rsx! { div { id: "terminal-host", onmounted: move |_|island.mount("terminal-host") } }
}

#[component]
fn App() -> Element {
    let mut logged = use_signal(|| !core::get("auth-token").is_empty());
    let mut status = use_signal(String::new);
    let mut lists = use_signal(String::new);
    let mut stream = use_signal(core::Stream::default);
    let mut composing = use_signal(|| false);
    let mut sent = use_signal(|| 0u32);
    let mut shown = use_signal(|| true);
    let mut island_state = use_signal(String::new);
    let island = use_hook(|| Rc::new(core::TerminalBridge::new()));
    let mut socket = use_signal(|| None::<core::MainSocket>);
    let session_island = island.clone();
    use_effect(move || {
        if logged() {
            core::restore_draft();
            wasm_bindgen_futures::spawn_local(async move {
                match core::lists().await {
                    Ok(data) => lists.set(data),
                    Err(error) => status.set(error),
                }
            });
            if socket.peek().is_none() {
                socket.set(Some(core::MainSocket::new(move |frame| {
                    if frame.kind == "stream_delta" {
                        stream.write().accept(&frame);
                    }
                    if frame.kind == "projects_updated" {
                        wasm_bindgen_futures::spawn_local(async move {
                            if let Ok(data) = core::lists().await {
                                lists.set(data);
                            }
                        });
                    }
                })));
                session_island.connect(&core::ws_url("/shell"));
            }
        }
    });
    let toggle_island = island.clone();
    let inspect_island = island.clone();
    let focus_island = island.clone();
    let write_island = island.clone();
    let reconnect_island = island.clone();
    rsx! {
        h1 { "F1 Dioxus probe" }
        p { "登录 + 会话列表 / S3 workload; 非生产页面" }
        label { "用户名" input { id: "username", "aria-label": "Username", value: "probe" } }
        label { "密码" input { id: "password", "aria-label": "Password", r#type: "password" } }
        button { id: "login", onclick: move |_| { spawn(async move {
            match core::login().await { Ok(_)=>{status.set(String::new()); logged.set(true);},Err(error)=>status.set(error) }
        }); }, "登录" }
        p { id: "status", "{status}" }
        if logged() {
            pre { id: "sessions", "{lists}" }
            textarea { id: "composer", "aria-label": "Message", placeholder: "消息",
                oncompositionstart: move |_|composing.set(true),
                oncompositionend: move |_|composing.set(false),
                oninput: move |_|core::save_draft(),
                onkeydown: move |e: KeyboardEvent| {
                    let legacy_ime = e.data().downcast::<web_sys::KeyboardEvent>().is_some_and(|raw|raw.key_code()==229);
                    if e.key() == Key::Enter && !e.modifiers().contains(Modifiers::SHIFT) && !*composing.peek() && !e.is_composing() && !legacy_ime {
                        e.prevent_default();
                        if !core::composer().value().trim().is_empty() { sent += 1; core::clear_draft(); }
                    }
                }
            }
            output { id: "sent", "{sent}" }
            pre { id: "stream", "data-received": "{stream.read().received}", "data-missing": "{stream.read().missing}", "{stream.read().text}" }
        }
        button { id: "toggle-island", onclick: move |_| { if shown() { toggle_island.detach(); } shown.set(!shown()); }, "切换终端" }
        button { id: "focus-island", onclick: move |_|focus_island.focus(), "聚焦终端" }
        button { id: "reenter-shell", onclick: move |_|reconnect_island.connect(&core::ws_url("/shell")), "手动重进终端" }
        button { id: "write-island", onclick: move |_|write_island.write("BUFFER_SENTINEL\r\n"), "写入缓冲标记" }
        button { id: "inspect-island", onclick: move |_|island_state.set(inspect_island.snapshot()), "读取终端状态" }
        pre { id: "island-state", "{island_state}" }
        if shown() { TerminalHost { island } }
        a { href: "/", "返回旧壳" }
    }
}
fn main() {
    dioxus::launch(App);
}

#[wasm_bindgen]
pub fn backoff(attempt: u32) -> u32 {
    core::reconnect_delay(attempt)
}
