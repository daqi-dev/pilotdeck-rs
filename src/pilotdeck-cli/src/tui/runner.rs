//! Interactive TUI runner backed by one remote gateway connection.
use super::{
    chat_layout::ChatLayout,
    commands::{self, Command, PermissionAction},
    permission_settings::{self, Patch},
    state::{self, State},
    transport::{Client, Error as TransportError},
};
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use std::{
    io::{self, IsTerminal, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::mpsc;

/// Bounded nonblocking writes share one absolute deadline for a complete draw.
struct DeadlineWriter {
    stdout: io::Stdout,
    deadline: std::time::Instant,
}
impl DeadlineWriter {
    fn new() -> Self {
        Self {
            stdout: io::stdout(),
            deadline: std::time::Instant::now() + Duration::from_secs(1),
        }
    }
    fn begin_frame(&mut self) {
        self.deadline = std::time::Instant::now() + Duration::from_secs(1);
    }
    fn wait_writable(&self) -> io::Result<()> {
        loop {
            let left = self
                .deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "TUI output timed out."))?;
            let mut fds = [rustix::event::PollFd::new(
                &self.stdout,
                rustix::event::PollFlags::OUT,
            )];
            let timeout = rustix::event::Timespec {
                tv_sec: left.as_secs() as i64,
                tv_nsec: i64::from(left.subsec_nanos()),
            };
            match rustix::event::poll(&mut fds, Some(&timeout)) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "TUI output timed out.",
                    ));
                }
                Ok(_) => return Ok(()),
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(io::Error::from(error)),
            }
        }
    }
}
impl Write for DeadlineWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            if std::time::Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "TUI output timed out.",
                ));
            }
            match self.stdout.write(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.wait_writable()?,
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        loop {
            if std::time::Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "TUI output timed out.",
                ));
            }
            match self.stdout.flush() {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.wait_writable()?,
                result => return result,
            }
        }
    }
}

struct TerminalGuard {
    stop: Arc<AtomicBool>,
    hooks: Vec<signal_hook::SigId>,
    watcher: Option<JoinHandle<()>>,
    stdout_flags: rustix::fs::OFlags,
}
impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        let flags = rustix::fs::fcntl_getfl(io::stdout()).map_err(io::Error::from)?;
        rustix::fs::fcntl_setfl(io::stdout(), flags | rustix::fs::OFlags::NONBLOCK)
            .map_err(io::Error::from)?;
        if let Err(error) = enable_raw_mode() {
            let _ = rustix::fs::fcntl_setfl(io::stdout(), flags);
            return Err(error);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(AtomicUsize::new(0));
        let mut hooks = Vec::new();
        for signal in [
            signal_hook::consts::SIGHUP,
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
        ] {
            match signal_hook::flag::register_usize(signal, Arc::clone(&seen), signal as usize) {
                Ok(hook) => hooks.push(hook),
                Err(error) => {
                    for hook in hooks {
                        signal_hook::low_level::unregister(hook);
                    }
                    let _ = disable_raw_mode();
                    let _ = rustix::fs::fcntl_setfl(io::stdout(), flags);
                    return Err(error);
                }
            }
        }
        let stopped = Arc::clone(&stop);
        let watcher = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let signal = seen.load(Ordering::Acquire);
                if signal != 0 {
                    let _ = disable_raw_mode();
                    let _ = rustix::fs::fcntl_setfl(io::stdout(), flags);
                    let _ = signal_hook::low_level::emulate_default_handler(signal as i32);
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Ok(Self {
            stop,
            hooks,
            watcher: Some(watcher),
            stdout_flags: flags,
        })
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for hook in self.hooks.drain(..) {
            signal_hook::low_level::unregister(hook);
        }
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), Show);
        let _ = rustix::fs::fcntl_setfl(io::stdout(), self.stdout_flags);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}
fn object(fields: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (name, value) in fields {
        object.insert(name.into(), value);
    }
    JsValue::Object(object)
}
fn text(value: Option<&JsValue>) -> String {
    value
        .and_then(JsValue::as_string)
        .map_or_else(String::new, |s| String::from_utf16_lossy(s.units()))
}
fn message(role: &str, text: &str) -> JsValue {
    object(vec![("role", role.into()), ("text", text.into())])
}
fn error_text(error: TransportError) -> String {
    match error {
        TransportError::Connection(e) => e,
        TransportError::Gateway(v) => {
            let msg = text(v.get("message"));
            if msg.is_empty() {
                "Gateway request failed.".into()
            } else {
                msg
            }
        }
        TransportError::ResourceLimit => "Gateway resource limit exceeded.".into(),
    }
}
enum Update {
    Event(u64, JsValue),
    Failed(u64, String),
    AbortResult(Result<(), String>),
    Done(u64),
    ToolOutput {
        context: ToolReadContext,
        result: Result<String, ()>,
        name: String,
    },
    NewSession(Result<String, String>),
    PermissionResult {
        generation: u64,
        id: String,
        result: Result<bool, String>,
    },
    Sessions {
        result: Result<Vec<JsValue>, String>,
        select: Option<usize>,
    },
    Switched {
        key: String,
        messages: Vec<JsValue>,
    },
}

pub async fn run(url: &str, token: &str, home: &Path, cwd: &Path) -> Result<u8, String> {
    if !io::stdout().is_terminal() {
        return Err("pilotdeck tui requires an interactive output terminal.".into());
    }
    let client = Client::connect(url, token).await.map_err(error_text)?;
    let settings = permission_settings::read(home).map_err(|e| e.to_string())?;
    let mut state = State::default();
    if settings.skip_permissions {
        state.mode = "bypassPermissions".into();
    }
    let mut session = format!("tui:project={}:default", cwd.to_string_lossy());
    let project = cwd.to_string_lossy().into_owned();
    let (updates, mut receive) = mpsc::channel::<Update>(1);
    let list_client = client.clone();
    let list_project = project.clone();
    tokio::spawn(async move {
        let _ = list_client
            .request(
                "list_sessions",
                object(vec![
                    ("projectKey", list_project.as_str().into()),
                    ("limit", JsValue::Number(8.0)),
                ]),
            )
            .await;
    });
    let _guard = match TerminalGuard::enter() {
        Ok(guard) => guard,
        Err(_) => {
            let _ = client.close().await;
            return Ok(1);
        }
    };
    let init_deadline = std::time::Instant::now() + Duration::from_secs(1);
    let mut terminal = loop {
        let mut writer = DeadlineWriter::new();
        writer.deadline = init_deadline;
        match Terminal::with_options(
            CrosstermBackend::new(writer),
            TerminalOptions {
                viewport: Viewport::Inline(18),
            },
        ) {
            Ok(terminal) => break terminal,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let mut writer = DeadlineWriter::new();
                writer.deadline = init_deadline;
                if let Err(error) = writer.wait_writable() {
                    let _ = writeln!(io::stdout(), "{error}");
                    drop(_guard);
                    let _ = client.close().await;
                    return Ok(1);
                }
            }
            Err(error) => {
                let _ = writeln!(io::stdout(), "{error}");
                drop(_guard);
                let _ = client.close().await;
                return Ok(1);
            }
        }
    };
    let mut input = String::new();
    let mut input_cursor = 0usize;
    let mut input_overflow = false;
    let mut help = false;
    let mut exit = false;
    let mut dirty = true;
    let mut new_pending = false;
    let mut permission_inflight = false;
    let mut abort_inflight = false;
    let mut connection_uncertain = false;
    let mut sessions: Vec<JsValue> = Vec::new();
    let mut sidebar = false;
    let mut sidebar_index = 0usize;
    let mut viewer: Option<JsValue> = None;
    let mut viewer_layout = ChatLayout::default();
    let mut viewer_scroll = 0usize;
    let mut focused_index: Option<usize> = None;
    let mut focus_scroll_pending = false;
    let mut tool_read_pending = false;
    let mut chat_scroll = 0usize;
    let mut chat_layout = ChatLayout::default();
    let mut generation = 0u64;
    let mut consumed_bytes = 0usize;
    let mut turn_task: Option<tokio::task::JoinHandle<()>> = None;
    let outcome: Result<(), String> = async {
    while !exit {
        if let Ok(update) = receive.try_recv() {
            dirty = true;
            match update {
                Update::Event(id, event) if id == generation => {
                    let size = pilotdeck_protocol::stringify_json(&event)
                        .map_err(|e| e.to_string())?
                        .map_or(0, |v| v.len());
                    if consumed_bytes.saturating_add(size) > 8 * 1024 * 1024
                        || state.messages.len() >= 4096
                        || state.pending_permissions.len() >= 64
                    {
                        if let Some(task) = turn_task.take() {
                            task.abort();
                        }
                        generation = generation.wrapping_add(1);
                        state.is_running = false;
                        state.pending_permissions.clear();
                        permission_inflight = false;
                        state.messages.push(message(
                            "error",
                            "TUI state limit reached; turn interrupted and pending permissions cleared.",
                        ));
                        spawn_abort(&mut abort_inflight,
                            client.clone(),
                            updates.clone(),
                            "abort_turn",
                            object(vec![("sessionKey", session.as_str().into())]),
                        );
                    } else {
                        consumed_bytes += size;
                        chat_scroll = 0;
                        if text(event.get("type")) == "tool_result_detail_available" {chat_layout.clear();}
                        state::apply(&mut state, &event).map_err(|e| e.to_string())?;
                    }
                }
                Update::Failed(id, error) if id == generation => {
                    state.is_running = false;
                    state.pending_permissions.clear();
                    permission_inflight = false;
                    state.messages.push(message("error", &error));
                }
                Update::Done(id) if id == generation => {
                    if state.is_running {
                        state.is_running = false;
                        state.pending_permissions.clear();
                        permission_inflight = false;
                        state.messages.push(message(
                            "error",
                            "Gateway stream ended without a terminal event.",
                        ));
                    }
                }
                Update::Event(_, _) | Update::Failed(_, _) | Update::Done(_) => {}
                Update::AbortResult(result) => {
                    abort_inflight=false;
                    if let Err(error)=result {
                        connection_uncertain=true;
                        if let Some(task)=turn_task.take(){task.abort();}
                        generation=generation.wrapping_add(1);
                        state.is_running=false;state.pending_permissions.clear();permission_inflight=false;
                        state.messages.push(message("error",&format!("Abort outcome unknown; reconnect required: {error}")));
                        let closing=client.clone();tokio::spawn(async move {let _=closing.close().await;});
                    }
                }
                Update::ToolOutput { context, result, name } => {
                    tool_read_pending=false;
                    if !context.matches(&session, generation) { continue; }
                    match result {
                        Err(())=>state.messages.push(message("error","Tool output exceeds the 1 MiB viewing limit.")),
                        Ok(_) if state.is_running || !state.pending_permissions.is_empty() => state.messages.push(message("error","Tool output is unavailable while a turn or permission is active.")),
                        Ok(content)=>if let Some(value)=open_tool_output(&mut terminal,&content,&name)? {viewer=Some(value);viewer_layout.clear();chat_layout.clear();viewer_scroll=0;},
                    }
                }
                Update::NewSession(result) => {
                    new_pending = false;
                    match result {
                        Ok(value) if !value.is_empty() => {
                            session = value.clone();
                            focused_index = None;
                            state.messages =
                                vec![message("system", &format!("New session: {value}"))];
                            consumed_bytes = 0; chat_layout.clear();
                        }
                        Ok(_) => state
                            .messages
                            .push(message("error", "Gateway returned no sessionKey.")),
                        Err(error) => state.messages.push(message("error", &error)),
                    }
                }
                Update::Sessions { result, select } => {
                    match result {
                        Ok(values) => sessions = values,
                        Err(error) if select.is_some() => {
                            new_pending = false;
                            state
                                .messages
                                .push(message("error", &format!("Command failed: {error}")));
                            continue;
                        }
                        Err(_) => {
                            new_pending = false;
                            sidebar = true;
                            sidebar_index = 0;
                            continue;
                        }
                    }
                    if let Some(index) = select.filter(|n| *n > 0 && *n <= sessions.len()) {
                        let target = &sessions[index - 1];
                        switch_session(
                            client.clone(),
                            updates.clone(),
                            project.clone(),
                            session_key(target),
                            text(target.get("summary")),
                        );
                    } else if select.is_some() {
                        request_sessions(client.clone(), updates.clone(), project.clone(), None);
                    } else {
                        new_pending = false;
                        sidebar = true;
                        sidebar_index = 0;
                    }
                }
                Update::Switched { key, messages } => {
                    new_pending = false;
                    sidebar = false;
                    session = key;
                    focused_index=None;
                    state.messages = messages;
                    consumed_bytes = 0; chat_layout.clear();
                }
                Update::PermissionResult {
                    generation: turn_id,
                    id,
                    result,
                } => {
                    if turn_id != generation {
                        continue;
                    }
                    permission_inflight = false;
                    match result {
                        Ok(true) => {
                            if state
                                .pending_permissions
                                .first()
                                .is_some_and(|p| text(p.get("requestId")) == id)
                            {
                                state.pending_permissions.remove(0);
                            }
                        }
                        Ok(false) => state.messages.push(message(
                            "error",
                            "Permission response was not delivered; retry or abort.",
                        )),
                        Err(error) => state.messages.push(message("error", &error)),
                    }
                }
            }
        }
        // Count all retained application data, including command output and RPC history.
        if viewer.is_some() && retained_bytes(&state, &sessions, viewer.as_ref(), &input).saturating_add(chat_layout.bytes()).saturating_add(viewer_layout.bytes()) > 8 * 1024 * 1024 {
            viewer=None;viewer_layout.clear();state.messages.push(message("error","Tool output exceeds the TUI state limit."));dirty=true;
        }
        if retained_bytes(&state, &sessions, viewer.as_ref(), &input).saturating_add(chat_layout.bytes()).saturating_add(viewer_layout.bytes()) > 8 * 1024 * 1024
            || state.messages.len() > 4096
        {
            if state.is_running || !state.pending_permissions.is_empty() {
                if let Some(task) = turn_task.take() {
                    task.abort();
                }
                generation = generation.wrapping_add(1);
                state.is_running = false;
                state.pending_permissions.clear();
                permission_inflight = false;
                spawn_abort(&mut abort_inflight,
                    client.clone(),
                    updates.clone(),
                    "abort_turn",
                    object(vec![("sessionKey", session.as_str().into())]),
                );
            }
            state.activity.clear();
            chat_layout.clear();
            sessions.clear();
            sidebar = false;
            viewer = None;viewer_layout.clear();focused_index=None;
            while !state.messages.is_empty()
                && (retained_bytes(&state, &sessions, None, &input) > 8 * 1024 * 1024 - 1024
                    || state.messages.len() >= 4096)
            {
                state.messages.pop();
            }
            state.messages.push(message("error","TUI state limit reached; newest content was removed and any active turn interrupted."));
            consumed_bytes = retained_bytes(&state, &sessions, None, &input);
        }
        if focused_index.is_some_and(|index| index >= state.messages.len()) { focused_index = None; }
        if dirty {
            let mut layout_ready=true;
            if let Some(content)=viewer.as_ref() {
                let width=terminal.size().map_err(|e|e.to_string())?.width;
                let budget=(8usize*1024*1024).saturating_sub(retained_bytes(&state,&sessions,viewer.as_ref(),&input)).saturating_sub(chat_layout.bytes());
                match viewer_layout.sync_step(std::slice::from_ref(content),width,budget) {
                    Ok(ready)=>layout_ready=ready,
                    Err(_)=>{viewer=None;viewer_layout.clear();state.messages.push(message("error","Tool output exceeds the TUI state limit."));layout_ready=false;dirty=true;}
                }
            } else if !sidebar && !help {
                let width=terminal.size().map_err(|e|e.to_string())?.width;
                let budget=(8usize*1024*1024).saturating_sub(retained_bytes(&state,&sessions,None,&input));
                let result=chat_layout.sync_step(&state.messages,width,budget);
                if result.is_err() {
                    if state.is_running || !state.pending_permissions.is_empty() {
                        if let Some(task)=turn_task.take(){task.abort();} generation=generation.wrapping_add(1);
                        state.is_running=false;state.pending_permissions.clear();permission_inflight=false;
                        spawn_abort(&mut abort_inflight,client.clone(),updates.clone(),"abort_turn",object(vec![("sessionKey",session.as_str().into())]));
                    }
                    chat_layout.clear();state.activity.clear();sessions.clear();viewer=None;sidebar=false;
                    let warning="TUI state limit reached; newest content removed and any active turn interrupted.";
                    if state.messages.last().is_some_and(|m|text(m.get("text"))==warning) {state.messages.pop();}
                    state.messages.pop();
                    state.messages.push(message("error",warning));
                    consumed_bytes=retained_bytes(&state,&sessions,None,&input);chat_scroll=0;dirty=true;
                }
                layout_ready=result.unwrap_or(false);
                if layout_ready && focus_scroll_pending {
                    let height=terminal.size().map_err(|e|e.to_string())?.height.min(18).saturating_sub(9) as usize;
                    if let Some(index)=focused_index {chat_scroll=chat_layout.scroll_for_message(index,height);}
                    focus_scroll_pending=false;
                }
            }
            if layout_ready {
            terminal.backend_mut().writer_mut().begin_frame();
            loop {
            let drawn=terminal.draw(|frame| {
            let areas = Layout::vertical([Constraint::Length(3),Constraint::Min(2),Constraint::Length(if state.pending_permissions.is_empty(){3}else{5}),Constraint::Length(3)]).split(frame.area());
            frame.render_widget(Paragraph::new(format!("PilotDeck ↗  v0.1.0\nPilotDeck · {} · {} · server {}",text(Some(&state.mode)), project,url)).block(Block::default().borders(Borders::BOTTOM)),areas[0]);
            if viewer.is_some() { viewer_layout.render_top(frame, areas[1], viewer_scroll); }
            else if !sidebar && !help { chat_layout.render(frame, areas[1], chat_scroll); }
            else {
                let mut lines: Vec<Line> = if sidebar {
                    sessions.iter().enumerate().map(|(i,s)| Line::raw(format!("{} {}  {}", if i==sidebar_index {">"} else {" "},session_key(s),text(s.get("summary"))))).collect()
                } else { Vec::new() };
                if help { lines.push(Line::raw("/new /mode /permissions /help /exit  |  Ctrl-C exits, Esc aborts")); }
                let visible = areas[1].height as usize; if lines.len() > visible {lines.truncate(visible);}
                frame.render_widget(Paragraph::new(lines).wrap(Wrap {trim:false}),areas[1]);
            }
            let status = if let Some(p) = state.pending_permissions.first() { let detail=permission_preview(p); let suffix=if detail.is_empty(){String::new()}else{format!(" — {detail}")}; let pending=if state.pending_permissions.len()>1{format!(" ({} pending)",state.pending_permissions.len())}else{String::new()};format!("Permission required{pending}: {}{suffix}\n[y] Allow once  [a] Allow + remember  [n] Deny  [Esc] Abort",text(p.get("toolName"))) } else if connection_uncertain { "Abort outcome unknown; reconnect required.".into() } else if abort_inflight { "Waiting for abort confirmation…".into() } else if state.is_running { "thinking…".into() } else if let Some(index)=focused_index {format!("Focused tool {}: {}  (Enter expands)",index+1,text(state.messages.get(index).and_then(|m|m.get("toolName"))))} else { String::new() };
            frame.render_widget(Paragraph::new(status).block(Block::default().borders(Borders::TOP)),areas[2]);
            frame.render_widget(Paragraph::new(if input.is_empty(){"Ask PilotDeck..."}else{&input}).block(Block::default().borders(Borders::TOP)),areas[3]);
            let cursor = if areas[3].width>0 && areas[3].height>1 {
                let width=Line::raw(&input[..input_cursor]).width().min(areas[3].width.saturating_sub(1) as usize) as u16;
                (areas[3].x+width,areas[3].y+1)
            } else { (frame.area().x,frame.area().y) };
            frame.set_cursor_position(cursor);
        }).map(|_|());
            match drawn {
                Ok(())=>break,
                Err(error) if error.kind()==io::ErrorKind::WouldBlock => terminal.backend().writer().wait_writable().map_err(|e|e.to_string())?,
                Err(error)=>return Err(error.to_string()),
            }
            }
            dirty = false;
            }
        }
        if event::poll(Duration::ZERO).map_err(|e| e.to_string())? {
            match event::read().map_err(|e| e.to_string())? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    dirty = true;
                    if viewer.is_some() {
                        let page=terminal.size().map_err(|e|e.to_string())?.height.min(18).saturating_sub(9).max(1) as usize;
                        let last=viewer_layout.total_rows().saturating_sub(page);
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => {viewer=None;viewer_layout.clear();chat_layout.clear();},
                            KeyCode::Up | KeyCode::Char('k') => viewer_scroll=viewer_scroll.saturating_sub(1),
                            KeyCode::Down | KeyCode::Char('j') => viewer_scroll=viewer_scroll.saturating_add(1).min(last),
                            KeyCode::PageUp => viewer_scroll=viewer_scroll.saturating_sub(page),
                            KeyCode::PageDown => viewer_scroll=viewer_scroll.saturating_add(page).min(last),
                            KeyCode::Char('g')=>viewer_scroll=0,
                            KeyCode::Char('G')=>viewer_scroll=last,
                            _ => {},
                        }
                        continue;
                    }
                    if sidebar && state.pending_permissions.is_empty() {
                        match key.code {
                            KeyCode::Esc => {
                                sidebar = false;
                                continue;
                            }
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                sidebar = false;
                                continue;
                            }
                            KeyCode::Up => {
                                sidebar_index = sidebar_index.saturating_sub(1);
                                continue;
                            }
                            KeyCode::Down => {
                                sidebar_index =
                                    (sidebar_index + 1).min(sessions.len().saturating_sub(1));
                                continue;
                            }
                            KeyCode::Enter
                                if input.is_empty()
                                    && !sessions.is_empty()
                                    && !state.is_running
                                    && !new_pending && !abort_inflight && !connection_uncertain && !tool_read_pending =>
                            {
                                let target = &sessions[sidebar_index];
                                let target_key = session_key(target);
                                new_pending = true;
                                switch_session(
                                    client.clone(),
                                    updates.clone(),
                                    project.clone(),
                                    target_key,
                                    text(target.get("summary")),
                                );
                                continue;
                            }
                            _ => {}
                        }
                    }
                    if !state.pending_permissions.is_empty() {
                        if abort_inflight && key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {exit=true;continue;}
                        if (permission_inflight || abort_inflight || connection_uncertain) && matches!(key.code, KeyCode::Char('y' | 'n' | 'a'))
                        {
                            continue;
                        }
                        let p = &state.pending_permissions[0];
                        let id = text(p.get("requestId"));
                        let (method, params) = match key.code {
                            KeyCode::Char('y') => (
                                "permission_decide",
                                object(vec![
                                    ("sessionKey", session.as_str().into()),
                                    ("requestId", id.as_str().into()),
                                    ("decision", "allow".into()),
                                    ("remember", JsValue::Bool(false)),
                                ]),
                            ),
                            KeyCode::Char('n') => (
                                "permission_decide",
                                object(vec![
                                    ("sessionKey", session.as_str().into()),
                                    ("requestId", id.as_str().into()),
                                    ("decision", "deny".into()),
                                    ("reason", "User denied in TUI".into()),
                                ]),
                            ),
                            KeyCode::Esc | KeyCode::Char('c')
                                if key.code == KeyCode::Esc
                                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
                            {
                                (
                                    "abort_turn",
                                    object(vec![("sessionKey", session.as_str().into())]),
                                )
                            }
                            KeyCode::Char('a') => {
                                let entry = permission_entry(p);
                                let settings = match permission_settings::read(home) {
                                    Ok(s) => s,
                                    Err(e) => {
                                        state.messages.push(message("error", &e.to_string()));
                                        continue;
                                    }
                                };
                                if !settings.allowed_tools.iter().any(|v| v.equals_str(&entry)) {
                                    let mut allowed = settings.allowed_tools;
                                    allowed.push(entry.into());
                                    if let Err(e) = permission_settings::write(
                                        home,
                                        Patch {
                                            allowed_tools: Some(allowed),
                                            ..Patch::default()
                                        },
                                    ) {
                                        state.messages.push(message("error", &e.to_string()));
                                        continue;
                                    }
                                }
                                (
                                    "permission_decide",
                                    object(vec![
                                        ("sessionKey", session.as_str().into()),
                                        ("requestId", id.as_str().into()),
                                        ("decision", "allow".into()),
                                        ("remember", JsValue::Bool(true)),
                                    ]),
                                )
                            }
                            _ => continue,
                        };
                        if method == "permission_decide" {
                            if permission_inflight {
                                continue;
                            }
                            permission_inflight = true;
                            let turn_id = generation;
                            let c = client.clone();
                            let tx = updates.clone();
                            tokio::spawn(async move {
                                let result = c
                                    .request(method, params)
                                    .await
                                    .map(|v| {
                                        v.get("delivered").and_then(JsValue::as_bool) == Some(true)
                                    })
                                    .map_err(error_text);
                                let _ = tx
                                    .send(Update::PermissionResult {
                                        generation: turn_id,
                                        id,
                                        result,
                                    })
                                    .await;
                            });
                        } else {
                            spawn_abort(&mut abort_inflight, client.clone(), updates.clone(), method, params);
                        }
                    } else if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if state.is_running && abort_inflight { exit = true; }
                        else if state.is_running {
                            spawn_abort(&mut abort_inflight,
                                client.clone(),
                                updates.clone(),
                                "abort_turn",
                                object(vec![("sessionKey", session.as_str().into())]),
                            );
                        } else {
                            exit = true;
                        }
                    } else {
                        match key.code {
                            KeyCode::Esc if state.is_running => spawn_abort(&mut abort_inflight,
                                client.clone(),
                                updates.clone(),
                                "abort_turn",
                                object(vec![("sessionKey", session.as_str().into())]),
                            ),
                            KeyCode::PageUp => chat_scroll = chat_scroll.saturating_add(10),
                            KeyCode::PageDown => chat_scroll = chat_scroll.saturating_sub(10),
                            KeyCode::Esc if help => help = false,
                            KeyCode::Esc => { focused_index = None; focus_scroll_pending = false; chat_scroll = 0; },
                            KeyCode::Tab | KeyCode::BackTab if input.is_empty() => {
                                let indices:Vec<usize>=state.messages.iter().enumerate().filter_map(|(i,m)|(text(m.get("role"))=="tool").then_some(i)).collect();
                                if !indices.is_empty() {
                                    let backwards=key.code==KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                                    focused_index=Some(match focused_index.and_then(|current|indices.iter().position(|i|*i==current)) {
                                        None=>if backwards {*indices.last().unwrap()} else {indices[0]},
                                        Some(pos)=>if backwards {indices[(pos+indices.len()-1)%indices.len()]} else {indices[(pos+1)%indices.len()]},
                                    });focus_scroll_pending=true;
                                }
                            }
                            KeyCode::Char('?') if input.is_empty() => help = !help,
                            KeyCode::Char(c)
                                if !key
                                    .modifiers
                                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                            {
                                if input.len() + c.len_utf8() <= 65536 {
                                    input.insert(input_cursor, c);
                                    input_cursor += c.len_utf8();focused_index=None;
                                } else {
                                    input_overflow = true;
                                    if !state.messages.last().is_some_and(|m| text(m.get("text")) == "Input limit reached.") {
                                        state.messages.push(message("error", "Input limit reached."));
                                    }
                                }
                            }
                            KeyCode::Backspace if input_cursor > 0 => {
                                let previous = input[..input_cursor]
                                    .char_indices()
                                    .next_back()
                                    .map_or(0, |(i, _)| i);
                                input.replace_range(previous..input_cursor, "");
                                input_cursor = previous;focused_index=None;
                            }
                            KeyCode::Left if input_cursor > 0 => {
                                input_cursor = input[..input_cursor]
                                    .char_indices()
                                    .next_back()
                                    .map_or(0, |(i, _)| i);
                            }
                            KeyCode::Right if input_cursor < input.len() => {
                                input_cursor +=
                                    input[input_cursor..].chars().next().unwrap().len_utf8();
                            }
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                input.clear();
                                input_cursor = 0;
                                input_overflow = false;
                            }
                            KeyCode::Enter if !state.is_running && !new_pending && !abort_inflight && !connection_uncertain && !tool_read_pending => {
                                if input_overflow {
                                    input.clear();
                                    input_cursor = 0;
                                    input_overflow = false;
                                    state.messages.push(message(
                                        "error",
                                        "Input exceeded the limit and was not submitted.",
                                    ));
                                    continue;
                                }
                                let prompt = input.trim_matches(js_space).to_owned();
                                input.clear();
                                input_cursor = 0;
                                if prompt.is_empty() {
                                    if let Some(index)=focused_index {
                                        if let Some(JsValue::Object(tool))=state.messages.get_mut(index) {
                                            let expanded=tool.get("expanded").and_then(JsValue::as_bool).unwrap_or(false);
                                            tool.insert("expanded".into(),JsValue::Bool(!expanded));chat_layout.clear();focus_scroll_pending=true;
                                        }
                                    } else if let Some(tool)=state.messages.iter().rev().find(|m| text(m.get("role"))=="tool" && (m.get("lineCount").and_then(JsValue::as_number).is_some_and(|n|n>4.0) || !text(m.get("resultPath")).is_empty())) {
                                        tool_read_pending=true;spawn_tool_read(tool,updates.clone(), ToolReadContext::new(&session, generation));
                                    }
                                    continue;
                                }
                                if let Some(command) = commands::parse(&prompt) {
                                    if state.messages.len() >= 4096
                                        && !matches!(command, Command::Clear | Command::Exit)
                                    {
                                        if !state.messages.last().is_some_and(|m| {
                                            text(m.get("text"))
                                                == "TUI state limit reached; use /clear."
                                        }) {
                                            state.messages.push(message(
                                                "error",
                                                "TUI state limit reached; use /clear.",
                                            ));
                                        }
                                        continue;
                                    }
                                    match command {
                                        Command::Exit => exit = true,
                                        Command::Help => help = !help,
                                        Command::Clear => {
                                            state.messages.clear();focused_index=None;
                                            consumed_bytes = 0; chat_layout.clear();
                                        }
                                        Command::Mode(mode) => {
                                            let desired = mode == "bypassPermissions";
                                            let result = permission_settings::read(home).and_then(
                                                |settings| {
                                                    if settings.skip_permissions == desired
                                                        && !desired
                                                    {
                                                        Ok(settings)
                                                    } else {
                                                        permission_settings::write(
                                                            home,
                                                            Patch {
                                                                skip_permissions: Some(desired),
                                                                ..Patch::default()
                                                            },
                                                        )
                                                    }
                                                },
                                            );
                                            match result {
                                                Ok(_) => {
                                                    state.mode = mode.into();
                                                    state.messages.push(message(
                                                        "system",
                                                        &format!("Mode: {mode}"),
                                                    ));
                                                }
                                                Err(e) => state
                                                    .messages
                                                    .push(message("error", &e.to_string())),
                                            }
                                        }
                                        Command::New => {
                                            new_pending = true;
                                            let c = client.clone();
                                            let tx = updates.clone();
                                            let p = project.clone();
                                            tokio::spawn(async move {
                                                match c
                                                    .request(
                                                        "new_session",
                                                        object(vec![
                                                            ("channelKey", "tui".into()),
                                                            ("projectKey", p.as_str().into()),
                                                        ]),
                                                    )
                                                    .await
                                                {
                                                    Ok(value) => {
                                                        let key = text(value.get("sessionKey"));
                                                        let _ = tx
                                                            .send(Update::NewSession(Ok(key)))
                                                            .await;
                                                    }
                                                    Err(e) => {
                                                        let _ = tx
                                                            .send(Update::NewSession(Err(
                                                                error_text(e),
                                                            )))
                                                            .await;
                                                    }
                                                }
                                            });
                                        }
                                        Command::Sidebar | Command::Switch(_) => {
                                            let select = if let Command::Switch(index) = command {
                                                Some(index)
                                            } else {
                                                None
                                            };
                                            new_pending = true;
                                            request_sessions(
                                                client.clone(),
                                                updates.clone(),
                                                project.clone(),
                                                select,
                                            );
                                        }
                                        Command::View(index) => {
                                            let tools: Vec<_> = state
                                                .messages
                                                .iter()
                                                .filter(|m| {
                                                    text(m.get("role")) == "tool"
                                                        && (m
                                                            .get("lineCount")
                                                            .and_then(JsValue::as_number)
                                                            .is_some_and(|n| n > 4.0)
                                                            || !text(m.get("resultPath"))
                                                                .is_empty()
                                                            || !text(m.get("fullText")).is_empty())
                                                })
                                                .collect();
                                            if tools.is_empty() {
                                                state.messages.push(message(
                                                    "system",
                                                    "No tool output to view.",
                                                ));
                                            } else {
                                                let target = index
                                                    .and_then(|n| n.checked_sub(1))
                                                    .and_then(|n| tools.get(n))
                                                    .copied()
                                                    .unwrap_or(tools[tools.len() - 1]);
                                                tool_read_pending=true;spawn_tool_read(target,updates.clone(), ToolReadContext::new(&session, generation));
                                            }
                                        }
                                        Command::Permissions(action) => {
                                            apply_permission_action(home, &mut state, action)
                                        }
                                        Command::Unknown(name) => state.messages.push(message(
                                            "error",
                                            &format!("Unknown command {name}"),
                                        )),
                                    }
                                } else {
                                    if consumed_bytes.saturating_add(prompt.len()) > 8 * 1024 * 1024
                                        || state.messages.len() >= 4096
                                    {
                                        state.messages.push(message(
                                            "error",
                                            "TUI state limit reached; use /clear.",
                                        ));
                                        continue;
                                    }
                                    consumed_bytes += prompt.len();
                                    generation = generation.wrapping_add(1);
                                    permission_inflight = false;
                                    let turn_id = generation;
                                    state.messages.push(message("user", &prompt));
                                    state.is_running = true;
                                    let c = client.clone();
                                    let tx = updates.clone();
                                    let session_key = session.clone();
                                    let p = project.clone();
                                    let mode = state.mode.clone();
                                    turn_task = Some(tokio::spawn(async move {
                                        match c
                                            .stream(
                                                "submit_turn",
                                                object(vec![
                                                    ("sessionKey", session_key.as_str().into()),
                                                    ("channelKey", "tui".into()),
                                                    ("projectKey", p.as_str().into()),
                                                    ("message", prompt.as_str().into()),
                                                    ("mode", mode),
                                                ]),
                                            )
                                            .await
                                        {
                                            Ok(mut stream) => {
                                                while let Some(event) = stream.next().await {
                                                    match event {
                                                        Ok(e) => {
                                                            if tx
                                                                .send(Update::Event(turn_id, e))
                                                                .await
                                                                .is_err()
                                                            {
                                                                return;
                                                            }
                                                        }
                                                        Err(e) => {
                                                            let _ = tx
                                                                .send(Update::Failed(
                                                                    turn_id,
                                                                    error_text(e),
                                                                ))
                                                                .await;
                                                            return;
                                                        }
                                                    }
                                                }
                                                let _ = tx.send(Update::Done(turn_id)).await;
                                            }
                                            Err(e) => {
                                                let _ = tx
                                                    .send(Update::Failed(turn_id, error_text(e)))
                                                    .await;
                                            }
                                        }
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Event::Resize(_, _) => {
                    dirty = true;
                }
                _ => {}
            }
        }
        tokio::task::yield_now().await;
        if receive.is_empty() {tokio::time::sleep(Duration::from_millis(2)).await;}
    }
    Ok(())
    }.await;
    if let Err(error) = &outcome {
        let _ = writeln!(io::stdout(), "{error}");
    }
    drop(terminal);
    drop(_guard);
    let _ = client.close().await;
    Ok(if outcome.is_ok() { 0 } else { 1 })
}
fn spawn_abort(
    inflight: &mut bool,
    client: Client,
    updates: mpsc::Sender<Update>,
    method: &'static str,
    params: JsValue,
) {
    if *inflight {
        return;
    }
    *inflight = true;
    tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(10), client.request(method, params))
            .await
            .map_err(|_| "Gateway abort timed out.".to_owned())
            .and_then(|result| result.map(|_| ()).map_err(error_text));
        let _ = updates.send(Update::AbortResult(result)).await;
    });
}

fn apply_permission_action(home: &Path, state: &mut State, action: PermissionAction) {
    let bypass = matches!(action, PermissionAction::Bypass);
    let settings = match permission_settings::read(home) {
        Ok(s) => s,
        Err(e) => {
            state.messages.push(message("error", &e.to_string()));
            return;
        }
    };
    let (patch, notice) = match action {
        PermissionAction::Show => {
            state.messages.push(message(
                "system",
                &format!(
                    "skipPermissions: {}\nallow: {}\ndeny: {}",
                    settings.skip_permissions,
                    display_list(&settings.allowed_tools),
                    display_list(&settings.disallowed_tools)
                ),
            ));
            return;
        }
        PermissionAction::Allow(entry) => {
            let mut v = settings.allowed_tools;
            v.push(JsString::from(entry.clone()));
            (
                Patch {
                    allowed_tools: Some(v),
                    ..Patch::default()
                },
                format!("Added allow: {entry}"),
            )
        }
        PermissionAction::Deny(entry) => {
            let mut v = settings.disallowed_tools;
            v.push(JsString::from(entry.clone()));
            (
                Patch {
                    disallowed_tools: Some(v),
                    ..Patch::default()
                },
                format!("Added deny: {entry}"),
            )
        }
        PermissionAction::Clear(entry) => {
            let e = JsString::from(entry.clone());
            (
                Patch {
                    allowed_tools: Some(
                        settings
                            .allowed_tools
                            .into_iter()
                            .filter(|v| v != &e)
                            .collect(),
                    ),
                    disallowed_tools: Some(
                        settings
                            .disallowed_tools
                            .into_iter()
                            .filter(|v| v != &e)
                            .collect(),
                    ),
                    ..Patch::default()
                },
                format!("Cleared: {entry}"),
            )
        }
        PermissionAction::Bypass => (
            Patch {
                skip_permissions: Some(true),
                ..Patch::default()
            },
            "Permissions bypassed globally (skipPermissions=true).".into(),
        ),
        PermissionAction::Usage => {
            state.messages.push(message(
                "error",
                "Usage: /permissions [allow|deny|clear <entry>|bypass]",
            ));
            return;
        }
        PermissionAction::Unknown(sub) => {
            state.messages.push(message(
                "error",
                &format!("Unknown /permissions subcommand: {sub}"),
            ));
            return;
        }
    };
    match permission_settings::write(home, patch) {
        Ok(_) => {
            if bypass {
                state.mode = "bypassPermissions".into();
            }
            state.messages.push(message("system", &notice));
        }
        Err(e) => state.messages.push(message("error", &e.to_string())),
    }
}
fn display_list(values: &[JsString]) -> String {
    if values.is_empty() {
        "(none)".into()
    } else {
        values
            .iter()
            .map(|s| String::from_utf16_lossy(s.units()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn permission_entry(p: &JsValue) -> String {
    let name = text(p.get("toolName"));
    if name != "bash" {
        return name;
    }
    let command = text(p.get("payload").and_then(|v| v.get("command")));
    let mut words = command.split(js_space).filter(|word| !word.is_empty());
    match (words.next(), words.next()) {
        (None, _) => "bash".into(),
        (Some("git"), Some(sub)) => format!("bash:git {sub}:*"),
        (Some(first), _) => format!("bash:{first}:*"),
    }
}

fn session_key(value: &JsValue) -> String {
    match value.get("sessionKey") {
        Some(JsValue::Null | JsValue::Undefined) | None => text(value.get("sessionId")),
        Some(value) => text(Some(value)),
    }
}
fn request_sessions(
    client: Client,
    updates: mpsc::Sender<Update>,
    project: String,
    select: Option<usize>,
) {
    tokio::spawn(async move {
        let result = client
            .request(
                "list_sessions",
                object(vec![
                    ("projectKey", project.as_str().into()),
                    ("limit", JsValue::Number(20.0)),
                ]),
            )
            .await
            .map(|v| match v.get("sessions") {
                Some(JsValue::Array(s)) => s.clone(),
                _ => vec![],
            })
            .map_err(error_text);
        let _ = updates.send(Update::Sessions { result, select }).await;
    });
}
fn switch_session(
    client: Client,
    updates: mpsc::Sender<Update>,
    project: String,
    key: String,
    summary: String,
) {
    tokio::spawn(async move {
        let resume_client = client.clone();
        let resume_key = key.clone();
        tokio::spawn(async move {
            let _ = resume_client
                .request(
                    "resume_session",
                    object(vec![("sessionKey", resume_key.as_str().into())]),
                )
                .await;
        });
        let history = tokio::time::timeout(
            Duration::from_secs(10),
            client.request(
                "read_session_messages",
                object(vec![
                    ("sessionKey", key.as_str().into()),
                    ("projectKey", project.as_str().into()),
                    ("limit", JsValue::Number(50.0)),
                    ("direction", "backward".into()),
                ]),
            ),
        )
        .await
        .unwrap_or_else(|_| {
            Err(TransportError::Connection(
                "History request timed out.".into(),
            ))
        });
        let fallback = if summary.is_empty() {
            key.as_str()
        } else {
            summary.as_str()
        };
        let messages = match history {
            Ok(result) => {
                let mut out = Vec::new();
                if let Some(JsValue::Array(rows)) = result.get("messages") {
                    for row in rows {
                        let role = text(row.get("role"));
                        let kind = text(row.get("kind"));
                        let content = text(row.get("text"));
                        if (role == "user" || role == "assistant")
                            && kind == "text"
                            && !content.is_empty()
                        {
                            out.push(message(&role, &content));
                        } else if role == "error" {
                            out.push(message(
                                "error",
                                if content.is_empty() {
                                    "error"
                                } else {
                                    &content
                                },
                            ));
                        } else if role == "assistant" && kind == "thinking" && !content.is_empty() {
                            if let Some(JsValue::Object(last)) = out.last_mut()
                                && last
                                    .get("role")
                                    .and_then(JsValue::as_string)
                                    .is_some_and(|r| r.equals_str("assistant"))
                            {
                                let mut units = last
                                    .get("thinking")
                                    .and_then(JsValue::as_string)
                                    .map_or_else(Vec::new, |s| s.units().to_vec());
                                if let Some(thinking) = row.get("text").and_then(JsValue::as_string)
                                {
                                    units.extend_from_slice(thinking.units());
                                }
                                last.insert(
                                    "thinking".into(),
                                    JsValue::String(JsString::from_utf16(units)),
                                );
                            }
                        } else if role == "tool" && kind == "tool_result" {
                            let fallback = if row.get("ok").and_then(JsValue::as_bool) == Some(true)
                            {
                                "ok"
                            } else {
                                "error"
                            };
                            let value = match row.get("text") {
                                Some(JsValue::Null | JsValue::Undefined) | None => fallback.into(),
                                Some(value) => value.clone(),
                            };
                            out.push(object(vec![
                                ("role", "tool".into()),
                                ("text", value),
                                ("ok", row.get("ok").cloned().unwrap_or(JsValue::Undefined)),
                                (
                                    "toolCallId",
                                    row.get("toolCallId").cloned().unwrap_or(JsValue::Undefined),
                                ),
                                (
                                    "toolName",
                                    row.get("toolName").cloned().unwrap_or(JsValue::Undefined),
                                ),
                            ]));
                        }
                    }
                }
                if out.is_empty() {
                    vec![message("system", &format!("Session: {fallback}"))]
                } else {
                    out
                }
            }
            Err(_) => vec![message(
                "system",
                &format!("Switched to session: {fallback}"),
            )],
        };
        let _ = updates.send(Update::Switched { key, messages }).await;
    });
}
#[derive(Debug)]
struct ToolReadContext {
    session: String,
    generation: u64,
}
impl ToolReadContext {
    fn new(session: &str, generation: u64) -> Self {
        Self {
            session: session.to_owned(),
            generation,
        }
    }
    fn matches(&self, session: &str, generation: u64) -> bool {
        self.session == session && self.generation == generation
    }
}

struct ToolRead {
    name: String,
    path: String,
    fallback: Option<String>,
}

fn tool_read_input(value: &JsValue) -> ToolRead {
    let bounded = |value: Option<&JsValue>, limit| match value {
        Some(JsValue::String(s)) if s.units().len() <= limit => {
            let value = String::from_utf16_lossy(s.units());
            (value.len() <= limit).then_some(value)
        }
        _ => None,
    };
    let fallback = match value.get("fullText") {
        Some(JsValue::String(_)) => bounded(value.get("fullText"), 1024 * 1024),
        _ => bounded(value.get("text"), 1024 * 1024)
            .or_else(|| (!matches!(value.get("text"), Some(JsValue::String(_)))).then(String::new)),
    };
    ToolRead {
        name: bounded(value.get("toolName"), 4096).unwrap_or_default(),
        path: bounded(value.get("resultPath"), 4096).unwrap_or_default(),
        fallback,
    }
}

fn tool_output(input: &ToolRead) -> Result<String, ()> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    if !input.path.is_empty() {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&input.path);
        if let Ok(file) = file
            && file.metadata().is_ok_and(|m| m.is_file())
        {
            let mut bytes = Vec::new();
            if file.take(1024 * 1024 + 1).read_to_end(&mut bytes).is_ok() {
                if bytes.len() > 1024 * 1024 {
                    return Err(());
                }
                let content = String::from_utf8_lossy(&bytes).into_owned();
                return if content.len() <= 1024 * 1024 {
                    Ok(content)
                } else {
                    Err(())
                };
            }
        }
    }
    input.fallback.clone().ok_or(())
}
fn spawn_tool_read(tool: &JsValue, updates: mpsc::Sender<Update>, context: ToolReadContext) {
    let input = tool_read_input(tool);
    std::thread::spawn(move || {
        let result = tool_output(&input);
        let _ = updates.blocking_send(Update::ToolOutput {
            context,
            result,
            name: input.name,
        });
    });
}
fn open_tool_output(
    terminal: &mut Terminal<CrosstermBackend<DeadlineWriter>>,
    content: &str,
    name: &str,
) -> Result<Option<JsValue>, String> {
    let lines = content.split('\n').count();
    let name = if name.is_empty() { "tool" } else { name };
    let formatted = format!("\n--- {name} output ---\n{content}\n---\n");
    if lines < 50 && content.len() <= 65536 {
        let width = terminal.size().map_err(|e| e.to_string())?.width;
        let height = Paragraph::new(formatted.as_str())
            .wrap(Wrap { trim: false })
            .line_count(width);
        if height <= 256 && height.saturating_mul(width as usize) <= 200_000 {
            terminal.backend_mut().writer_mut().begin_frame();
            terminal
                .insert_before(height as u16, |buffer| {
                    Paragraph::new(formatted.as_str())
                        .wrap(Wrap { trim: false })
                        .render(buffer.area, buffer)
                })
                .map_err(|e| e.to_string())?;
            return Ok(None);
        }
    }
    Ok(Some(message("", content)))
}

fn js_space(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn retained_bytes(
    state: &State,
    sessions: &[JsValue],
    viewer: Option<&JsValue>,
    input: &str,
) -> usize {
    let mut total = input.len();
    let mut stack: Vec<&JsValue> = state
        .messages
        .iter()
        .chain(&state.activity)
        .chain(&state.pending_permissions)
        .chain(sessions)
        .chain(viewer)
        .collect();
    while let Some(value) = stack.pop() {
        total = total.saturating_add(32);
        match value {
            JsValue::String(s) => total = total.saturating_add(s.len_utf16().saturating_mul(2)),
            JsValue::Array(values) => stack.extend(values),
            JsValue::Object(object) => {
                for (key, value) in object.entries() {
                    total = total.saturating_add(key.len_utf16().saturating_mul(2));
                    stack.push(value);
                }
            }
            _ => {}
        }
        if total > 8 * 1024 * 1024 {
            break;
        }
    }
    total
}

fn permission_preview(permission: &JsValue) -> String {
    let name = text(permission.get("toolName"));
    let Some(payload) = permission.get("payload").and_then(JsValue::as_object) else {
        return String::new();
    };
    let keys: &[&str] = match name.as_str() {
        "bash" => &["command"],
        "write_file" | "writeFile" | "edit_file" | "editFile" | "str_replace_editor" => {
            &["path", "file_path", "filePath"]
        }
        "agent" => &["description", "task", "prompt"],
        "web_search" | "webSearch" => &["query", "search_term"],
        "web_fetch" | "webFetch" => &["url"],
        _ => &[],
    };
    let value = if keys.is_empty() {
        payload
            .own_entries()
            .into_iter()
            .find_map(|(_, v)| v.as_string())
    } else {
        keys.iter()
            .find_map(|k| {
                payload
                    .get(k)
                    .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
            })
            .and_then(JsValue::as_string)
    };
    match value {
        None => String::new(),
        Some(value) if value.len_utf16() > 60 => {
            format!("{}...", String::from_utf16_lossy(&value.units()[..57]))
        }
        Some(value) => String::from_utf16_lossy(value.units()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_wraps_to_tail_and_can_scroll_to_start() {
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(12, 3)).unwrap();
        let messages = vec![message("assistant", "begin line\nmiddle line\nfinal line")];
        let mut layout = ChatLayout::default();
        layout.sync(&messages, 12, 8 * 1024 * 1024).unwrap();
        terminal.draw(|f| layout.render(f, f.area(), 0)).unwrap();
        let tail: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(tail.contains("final"), "tail must show final text: {tail}");
        terminal.draw(|f| layout.render(f, f.area(), 100)).unwrap();
        let start: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            start.contains("begin"),
            "scroll must reach earlier text: {start}"
        );
    }
    #[test]
    fn long_chat_tail_and_permission_preview_are_preserved() {
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(16, 3)).unwrap();
        let value = format!("{}FINAL", "x\n".repeat(70000));
        let messages = vec![message("assistant", &value)];
        let mut layout = ChatLayout::default();
        layout.sync(&messages, 16, 8 * 1024 * 1024).unwrap();
        terminal.draw(|f| layout.render(f, f.area(), 0)).unwrap();
        let output: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(output.contains("FINAL"), "long transcript tail: {output}");
        let p = object(vec![
            ("toolName", "bash".into()),
            ("payload", object(vec![("command", "git status".into())])),
        ]);
        assert_eq!(permission_preview(&p), "git status");
    }
    #[tokio::test]
    async fn delayed_tool_output_is_rejected_after_switch() {
        let (gate, wait) = tokio::sync::oneshot::channel();
        let (tx, mut rx) = mpsc::channel(1);
        let worker = tokio::spawn(async move {
            wait.await.unwrap();
            tx.send(Update::ToolOutput {
                context: ToolReadContext::new("old-session", 7),
                result: Ok("old contents".into()),
                name: "tool".into(),
            })
            .await
            .unwrap();
        });
        // The response is held until after the selected session changes.
        let current_session = "new-session";
        gate.send(()).unwrap();
        let Update::ToolOutput { context, .. } = rx.recv().await.unwrap() else {
            panic!("unexpected update")
        };
        assert!(!context.matches(current_session, 7));
        worker.await.unwrap();
    }
    #[test]
    fn stale_tool_output_is_rejected_after_session_or_turn_change() {
        let context = ToolReadContext::new("first", 7);
        assert!(context.matches("first", 7));
        assert!(!context.matches("second", 7));
        assert!(!context.matches("first", 8));
    }
    #[test]
    fn tool_output_is_bounded_and_preserves_empty_full_text() {
        let value = object(vec![("text", "preview".into()), ("fullText", "".into())]);
        assert_eq!(tool_output(&tool_read_input(&value)).unwrap(), "");
        let large_text = "x".repeat(1024 * 1024 + 1);
        let large = object(vec![("fullText", large_text.as_str().into())]);
        assert!(tool_output(&tool_read_input(&large)).is_err());
        let fallback = object(vec![
            ("resultPath", "/no/such/tool-output".into()),
            ("text", "preview".into()),
        ]);
        assert_eq!(tool_output(&tool_read_input(&fallback)).unwrap(), "preview");
    }
    #[test]
    fn expanded_tool_changes_rendered_text_and_viewer_wraps_long_lines() {
        use ratatui::backend::TestBackend;
        let tool = object(vec![
            ("role", "tool".into()),
            ("text", "preview".into()),
            ("fullText", "expanded".into()),
            ("expanded", JsValue::Bool(true)),
        ]);
        let mut layout = ChatLayout::default();
        layout.sync(&[tool], 12, 8 * 1024 * 1024).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(12, 3)).unwrap();
        terminal
            .draw(|f| layout.render_top(f, f.area(), 0))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("expanded"), "{screen}");
        let viewer = message("", &format!("{}END", "x".repeat(30)));
        layout.clear();
        layout.sync(&[viewer], 12, 8 * 1024 * 1024).unwrap();
        terminal
            .draw(|f| layout.render_top(f, f.area(), 0))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("END"), "{screen}");
    }
    #[test]
    fn empty_session_key_does_not_fall_back() {
        let session = object(vec![
            ("sessionKey", "".into()),
            ("sessionId", "other".into()),
        ]);
        assert_eq!(session_key(&session), "");
    }
}
