//! Terminal input for setup's default readline interface.
use rustix::termios::{
    self, ControlModes, InputModes, LocalModes, OptionalActions, SpecialCodeIndex, Termios,
};
use signal_hook::{
    SigId,
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
};
use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

pub(super) struct TerminalGuard {
    original: Termios,
    hooks: Vec<SigId>,
    stop: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}
impl TerminalGuard {
    pub fn enter() -> Option<Self> {
        let original = termios::tcgetattr(std::io::stdin()).ok()?;
        let mut raw = original.clone();
        raw.input_modes.remove(
            InputModes::BRKINT
                | InputModes::ICRNL
                | InputModes::INPCK
                | InputModes::ISTRIP
                | InputModes::IXON,
        );
        raw.control_modes.insert(ControlModes::CS8);
        raw.local_modes
            .remove(LocalModes::ECHO | LocalModes::ICANON | LocalModes::IEXTEN | LocalModes::ISIG);
        raw.special_codes[SpecialCodeIndex::VMIN] = 1;
        raw.special_codes[SpecialCodeIndex::VTIME] = 0;
        termios::tcsetattr(std::io::stdin(), OptionalActions::Now, &raw).ok()?;
        let stop = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(AtomicUsize::new(0));
        let mut hooks = Vec::new();
        for signal in [SIGHUP, SIGINT, SIGTERM] {
            match signal_hook::flag::register_usize(signal, observed.clone(), signal as usize) {
                Ok(hook) => hooks.push(hook),
                Err(_) => {
                    for hook in hooks {
                        signal_hook::low_level::unregister(hook);
                    }
                    let _ = termios::tcsetattr(std::io::stdin(), OptionalActions::Now, &original);
                    return None;
                }
            }
        }
        let settings = original.clone();
        let stopped = stop.clone();
        let watcher = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let signal = observed.load(Ordering::Acquire);
                if signal != 0 {
                    let _ = termios::tcsetattr(std::io::stdin(), OptionalActions::Now, &settings);
                    let _ = signal_hook::low_level::emulate_default_handler(signal as i32);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        Some(Self {
            original,
            hooks,
            stop,
            watcher: Some(watcher),
        })
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for hook in self.hooks.drain(..) {
            signal_hook::low_level::unregister(hook);
        }
        let _ = termios::tcsetattr(std::io::stdin(), OptionalActions::Now, &self.original);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

pub(super) enum Input {
    Line(String),
    Abort(&'static str),
}
#[derive(Default)]
pub(super) struct Editor {
    line: String,
    cursor: usize,
    escape: Vec<u8>,
    utf8: Vec<u8>,
    after_cr: bool,
    history: Vec<String>,
    history_index: Option<usize>,
}
impl Editor {
    pub fn feed(&mut self, byte: u8, prompt: &str) -> Option<Input> {
        if !self.escape.is_empty() {
            self.escape.push(byte);
            if self.escape.len() == 2 && byte != b'[' && byte != b'O' {
                self.escape.clear();
                return None;
            }
            if self.escape.len() >= 3 && (byte.is_ascii_alphabetic() || byte == b'~') {
                let key = std::mem::take(&mut self.escape);
                match key.as_slice() {
                    b"\x1b[D" => self.left(),
                    b"\x1b[C" => self.right(),
                    b"\x1b[H" | b"\x1bOH" | b"\x1b[1~" => self.home(),
                    b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" => self.end(),
                    b"\x1b[3~" => self.delete(prompt),
                    b"\x1b[A" => self.history_move(true, prompt),
                    b"\x1b[B" => self.history_move(false, prompt),
                    _ => {}
                }
            } else if self.escape.len() > 20 {
                self.escape.clear();
            }
            return None;
        }
        if byte == 0x1b {
            self.escape.push(byte);
            return None;
        }
        if byte == b'\n' && self.after_cr {
            self.after_cr = false;
            return None;
        }
        self.after_cr = false;
        match byte {
            b'\r' | b'\n' => {
                write("\r\n");
                self.after_cr = byte == b'\r';
                let answer = std::mem::take(&mut self.line);
                self.cursor = 0;
                self.history_index = None;
                if !answer.is_empty() && self.history.last() != Some(&answer) {
                    self.history.push(answer.clone());
                }
                Some(Input::Line(answer))
            }
            3 => Some(Input::Abort("Aborted with Ctrl+C")),
            4 if self.line.is_empty() => Some(Input::Abort("Aborted with Ctrl+D")),
            4 => {
                self.delete(prompt);
                None
            }
            1 => {
                self.home();
                None
            }
            5 => {
                self.end();
                None
            }
            8 | 127 => {
                if self.cursor > 0 {
                    let previous = self.previous();
                    self.line.drain(previous..self.cursor);
                    self.cursor = previous;
                    self.refresh(prompt);
                }
                None
            }
            11 => {
                self.line.truncate(self.cursor);
                self.refresh(prompt);
                None
            }
            21 => {
                self.line.drain(..self.cursor);
                self.cursor = 0;
                self.refresh(prompt);
                None
            }
            23 => {
                let prefix = &self.line[..self.cursor];
                let trimmed = prefix.trim_end();
                let previous = trimmed
                    .rfind(char::is_whitespace)
                    .map_or(0, |i| i + trimmed[i..].chars().next().unwrap().len_utf8());
                self.line.drain(previous..self.cursor);
                self.cursor = previous;
                self.refresh(prompt);
                None
            }
            12 => {
                write("\x1b[1;1H\x1b[0J");
                self.refresh(prompt);
                None
            }
            b if b >= 32 || b == b'\t' => {
                self.utf8.push(b);
                match std::str::from_utf8(&self.utf8) {
                    Ok(value) => {
                        let value = value.to_owned();
                        self.utf8.clear();
                        let append = self.cursor == self.line.len();
                        self.line.insert_str(self.cursor, &value);
                        self.cursor += value.len();
                        if append {
                            write(&value);
                        } else {
                            self.refresh(prompt);
                        }
                    }
                    Err(error) if error.error_len().is_some() => {
                        self.utf8.clear();
                        self.line.insert(self.cursor, '\u{fffd}');
                        self.cursor += '\u{fffd}'.len_utf8();
                        self.refresh(prompt);
                    }
                    _ => {}
                }
                None
            }
            _ => None,
        }
    }
    fn previous(&self) -> usize {
        self.line[..self.cursor]
            .char_indices()
            .last()
            .map_or(0, |(i, _)| i)
    }
    fn next(&self) -> usize {
        self.cursor
            + self.line[self.cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8)
    }
    fn left(&mut self) {
        if self.cursor > 0 {
            let previous = self.previous();
            let width = width(&self.line[previous..self.cursor]);
            self.cursor = previous;
            if width > 0 {
                write(&format!("\x1b[{width}D"));
            }
        }
    }
    fn right(&mut self) {
        if self.cursor < self.line.len() {
            let next = self.next();
            let width = width(&self.line[self.cursor..next]);
            self.cursor = next;
            if width > 0 {
                write(&format!("\x1b[{width}C"));
            }
        }
    }
    fn home(&mut self) {
        let width = width(&self.line[..self.cursor]);
        self.cursor = 0;
        if width > 0 {
            write(&format!("\x1b[{width}D"));
        }
    }
    fn end(&mut self) {
        let width = width(&self.line[self.cursor..]);
        self.cursor = self.line.len();
        if width > 0 {
            write(&format!("\x1b[{width}C"));
        }
    }
    fn delete(&mut self, prompt: &str) {
        if self.cursor < self.line.len() {
            let next = self.next();
            self.line.drain(self.cursor..next);
            self.refresh(prompt);
        }
    }
    fn history_move(&mut self, up: bool, prompt: &str) {
        if self.history.is_empty() {
            return;
        }
        let index = match (up, self.history_index) {
            (true, None) => self.history.len() - 1,
            (true, Some(index)) => index.saturating_sub(1),
            (false, Some(index)) if index + 1 < self.history.len() => index + 1,
            _ => {
                self.history_index = None;
                self.line.clear();
                self.cursor = 0;
                self.refresh(prompt);
                return;
            }
        };
        self.history_index = Some(index);
        self.line = self.history[index].clone();
        self.cursor = self.line.len();
        self.refresh(prompt);
    }
    fn refresh(&self, prompt: &str) {
        write(&format!(
            "\x1b[1G\x1b[0J{prompt}{}\x1b[{}G",
            self.line,
            width(prompt) + width(&self.line[..self.cursor]) + 1
        ));
    }
}
fn write(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}
pub(super) fn width(text: &str) -> usize {
    text.chars()
        .map(|c| match c as u32 {
            0..=31 | 0x7f..=0x9f | 0x300..=0x36f | 0x200b..=0x200f | 0xfe00..=0xfe0f | 0xfeff => 0,
            0x1100..=0x115f
            | 0x2329..=0x232a
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f000..=0x1faff
            | 0x20000..=0x3fffd => 2,
            _ => 1,
        })
        .sum()
}
