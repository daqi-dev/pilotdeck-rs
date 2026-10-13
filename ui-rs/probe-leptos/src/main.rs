use leptos::prelude::*;
use send_wrapper::SendWrapper;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
#[path = "../../shared/core.rs"]
mod core;

#[component]
fn TerminalHost(island: SendWrapper<Rc<core::TerminalBridge>>) -> impl IntoView {
    let host = NodeRef::<leptos::html::Div>::new();
    let mount = island.clone();
    Effect::new(move |_| {
        if host.get().is_some() {
            mount.mount("terminal-host");
        }
    });
    on_cleanup(move || island.detach());
    view! { <div id="terminal-host" node_ref=host></div> }
}

#[component]
fn App() -> impl IntoView {
    let logged = RwSignal::new(!core::get("auth-token").is_empty());
    let status = RwSignal::new(String::new());
    let lists = RwSignal::new(String::new());
    let stream = RwSignal::new(core::Stream::default());
    let composing = RwSignal::new(false);
    let sent = RwSignal::new(0u32);
    let shown = RwSignal::new(true);
    let island_state = RwSignal::new(String::new());
    let island = SendWrapper::new(Rc::new(core::TerminalBridge::new()));
    let socket = StoredValue::new_local(None::<core::MainSocket>);
    let session_island = island.clone();
    Effect::new(move |_| {
        if logged.get() {
            core::restore_draft();
            wasm_bindgen_futures::spawn_local(async move {
                match core::lists().await {
                    Ok(data) => lists.set(data),
                    Err(error) => status.set(error),
                }
            });
            if socket.with_value(|s| s.is_none()) {
                socket.set_value(Some(core::MainSocket::new(move |frame| {
                    if frame.kind == "stream_delta" {
                        stream.update(|s| s.accept(&frame));
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
    let login = move |_| {
        wasm_bindgen_futures::spawn_local(async move {
            match core::login().await {
                Ok(_) => {
                    status.set(String::new());
                    logged.set(true);
                }
                Err(error) => status.set(error),
            }
        })
    };
    let toggle_island = island.clone();
    let toggle = move |_| {
        if shown.get() {
            toggle_island.detach();
        }
        shown.update(|v| *v = !*v);
    };
    let inspect_island = island.clone();
    let focus_island = island.clone();
    let write_island = island.clone();
    let reconnect_island = island.clone();
    view! {
        <h1>"F1 Leptos probe"</h1>
        <p>"登录 + 会话列表 / S3 workload; 非生产页面"</p>
        <label>"用户名"<input id="username" aria-label="Username" value="probe"/></label>
        <label>"密码"<input id="password" aria-label="Password" type="password"/></label>
        <button id="login" on:click=login>"登录"</button>
        <p id="status">{move ||status.get()}</p>
        <Show when=move ||logged.get()>
            <pre id="sessions">{move ||lists.get()}</pre>
            <textarea id="composer" aria-label="Message" placeholder="消息"
                on:compositionstart=move |_|composing.set(true)
                on:compositionend=move |_|composing.set(false)
                on:input=move |_|core::save_draft()
                on:keydown=move |e: web_sys::KeyboardEvent| {
                    if e.key() == "Enter" && !e.shift_key() && !composing.get_untracked() && !e.is_composing() && e.key_code() != 229 {
                        e.prevent_default();
                        if !core::composer().value().trim().is_empty() { sent.update(|n|*n+=1); core::clear_draft(); }
                    }
                }></textarea>
            <output id="sent">{move ||sent.get()}</output>
            <pre id="stream" data-received=move ||stream.get().received data-missing=move ||stream.get().missing>{move ||stream.get().text}</pre>
        </Show>
        <button id="toggle-island" on:click=toggle>"切换终端"</button>
        <button id="focus-island" on:click=move |_|focus_island.focus()>"聚焦终端"</button>
        <button id="reenter-shell" on:click=move |_|reconnect_island.connect(&core::ws_url("/shell"))>"手动重进终端"</button>
        <button id="write-island" on:click=move |_|write_island.write("BUFFER_SENTINEL\r\n")>"写入缓冲标记"</button>
        <button id="inspect-island" on:click=move |_|island_state.set(inspect_island.snapshot())>"读取终端状态"</button>
        <pre id="island-state">{move ||island_state.get()}</pre>
        {move ||if shown.get() { Some(view! { <TerminalHost island=island.clone()/> }) } else { None }}
        <a href="/">"返回旧壳"</a>
    }
}
fn main() {
    leptos::mount::mount_to_body(App);
}

#[wasm_bindgen]
pub fn backoff(attempt: u32) -> u32 {
    core::reconnect_delay(attempt)
}
