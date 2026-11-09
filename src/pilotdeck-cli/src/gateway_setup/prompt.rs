use crate::{Failure, paths::js_trim};
use std::{
    io::{IsTerminal, Read, Write},
    sync::{Arc, Mutex, mpsc},
};

#[derive(Debug)]
pub(super) enum SetupError {
    Eof,
    Failure(Failure),
}
pub(super) type SetupResult<T> = Result<T, SetupError>;
impl From<Failure> for SetupError {
    fn from(value: Failure) -> Self {
        Self::Failure(value)
    }
}

pub(super) trait Prompt {
    fn settle(&mut self) {}
    fn line(&mut self, value: &str) -> SetupResult<()>;
    fn question(&mut self, prompt: &str) -> SetupResult<String>;
    fn ask(&mut self, prompt: &str) -> SetupResult<String> {
        self.question(prompt)
            .map(|value| js_trim(&value).to_lowercase())
    }
    fn text(&mut self, prompt: &str) -> SetupResult<String> {
        self.question(prompt)
            .map(|value| js_trim(&value).to_owned())
    }
}

#[derive(Default)]
struct InputState {
    pending: Option<mpsc::Sender<SetupResult<String>>>,
    closed: bool,
    prompt: String,
}
/// readline delivers lines only to the currently pending question. Lines in
/// the same input callback after its first answer are not queued for later
/// questions; holding the state lock for a read batch preserves that behavior.
pub(super) struct Console {
    input: Arc<Mutex<InputState>>,
    started: bool,
    terminal: bool,
    _mode: Option<super::terminal::TerminalGuard>,
}
impl Console {
    pub fn new() -> Self {
        let terminal = std::io::stdout().is_terminal();
        let mode = if terminal {
            super::terminal::TerminalGuard::enter()
        } else {
            None
        };
        Self {
            terminal,
            _mode: mode,
            input: Arc::new(Mutex::new(InputState::default())),
            started: false,
        }
    }
}
impl Prompt for Console {
    fn settle(&mut self) {
        if !self.terminal {
            let stdin = std::io::stdin();
            let mut fds = [rustix::event::PollFd::new(
                &stdin,
                rustix::event::PollFlags::IN,
            )];
            // Awaiting the optional SDK import lets readline observe an
            // already closed pipe before the next question.
            if rustix::event::poll(
                &mut fds,
                Some(&rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                }),
            )
            .is_ok()
                && fds[0].revents().contains(rustix::event::PollFlags::HUP)
            {
                self.input.lock().unwrap_or_else(|p| p.into_inner()).closed = true;
            }
        }
    }
    fn line(&mut self, value: &str) -> SetupResult<()> {
        let _ = writeln!(std::io::stdout().lock(), "{value}");
        Ok(())
    }
    fn question(&mut self, prompt: &str) -> SetupResult<String> {
        let (send, receive) = mpsc::channel();
        {
            let mut input = self.input.lock().unwrap_or_else(|p| p.into_inner());
            if input.closed {
                return Err(Failure::Error("readline was closed".into()).into());
            }
            input.pending = Some(send);
            input.prompt = prompt.to_owned();
            let mut out = std::io::stdout().lock();
            if self.terminal && std::env::var("TERM").as_deref() != Ok("dumb") {
                let _ = write!(
                    out,
                    "\x1b[1G\x1b[0J{prompt}\x1b[{}G",
                    super::terminal::width(prompt) + 1
                );
            } else {
                let _ = write!(out, "{prompt}");
            }
            let _ = out.flush();
        }
        if !self.started {
            self.started = true;
            let state = self.input.clone();
            let terminal = self.terminal;
            std::thread::spawn(move || {
                let mut stdin = std::io::stdin().lock();
                let mut bytes = [0; 4096];
                let mut line = Vec::new();
                let mut after_cr = false;
                let mut editor = super::terminal::Editor::default();
                loop {
                    let count = stdin.read(&mut bytes).unwrap_or_default();
                    let mut input = state.lock().unwrap_or_else(|p| p.into_inner());
                    if count == 0 {
                        if !line.is_empty()
                            && let Some(send) = input.pending.take()
                        {
                            let _ = send.send(Ok(String::from_utf8_lossy(&line).into_owned()));
                        }
                        input.closed = true;
                        input.pending.take();
                        break;
                    }
                    for &byte in &bytes[..count] {
                        if terminal {
                            match editor.feed(byte, &input.prompt) {
                                Some(super::terminal::Input::Line(line)) => {
                                    if let Some(send) = input.pending.take() {
                                        let _ = send.send(Ok(line));
                                    }
                                }
                                Some(super::terminal::Input::Abort(message)) => {
                                    input.closed = true;
                                    if let Some(send) = input.pending.take() {
                                        let _ =
                                            send.send(Err(Failure::Error(message.into()).into()));
                                    }
                                    return;
                                }
                                None => {}
                            }
                            continue;
                        }
                        if byte == b'\n' && after_cr {
                            after_cr = false;
                            continue;
                        }
                        if byte == b'\n' || byte == b'\r' {
                            if let Some(send) = input.pending.take() {
                                let _ = send.send(Ok(String::from_utf8_lossy(&line).into_owned()));
                            }
                            line.clear();
                            after_cr = byte == b'\r';
                        } else {
                            after_cr = false;
                            line.push(byte);
                        }
                    }
                }
            });
        }
        receive.recv().unwrap_or(Err(SetupError::Eof))
    }
}
