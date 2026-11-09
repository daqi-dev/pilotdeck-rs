//! Bounded terminal byte decoder. Never hands an incomplete paste to the UI.
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::{
    collections::VecDeque,
    io,
    time::{Duration, Instant},
};
const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";
const MAX: usize = 65536;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Paste,
    Discard,
    CsiDiscard,
}
pub(super) enum InputEvent {
    Terminal(Event),
    PasteStarted,
    TextBatch(String),
    PasteOverflow,
    PasteCancelled,
    Eof,
}
pub(super) struct TerminalInput {
    bytes: VecDeque<u8>,
    paste: Vec<u8>,
    mode: Mode,
    esc_since: Option<Instant>,
    eof: bool,
    cancelled: bool,
    after_esc: bool,
}
impl Default for TerminalInput {
    fn default() -> Self {
        Self {
            bytes: VecDeque::new(),
            paste: Vec::new(),
            mode: Mode::Normal,
            esc_since: None,
            eof: false,
            cancelled: false,
            after_esc: false,
        }
    }
}
fn key(code: KeyCode, mods: KeyModifiers) -> InputEvent {
    InputEvent::Terminal(Event::Key(KeyEvent::new(code, mods)))
}
fn raw_batch(data: &[u8]) -> Option<&str> {
    if data.len() <= 1 {
        return None;
    }
    let value = std::str::from_utf8(data).ok()?;
    value
        .chars()
        .all(|c| !c.is_control() || c == '\r' || c == '\n')
        .then_some(value)
}
impl TerminalInput {
    pub fn poll(&mut self) -> io::Result<Option<InputEvent>> {
        if self.bytes.len() < 4096 && !self.eof {
            let mut buf = [0u8; 4096];
            match rustix::io::read(io::stdin(), &mut buf) {
                Ok(0) => self.eof = true,
                Ok(n) => {
                    let data = &buf[..n];
                    if self.mode == Mode::Normal
                        && self.bytes.is_empty()
                        && !self.after_esc
                        && let Some(raw) = raw_batch(data)
                    {
                        return Ok(Some(InputEvent::TextBatch(raw.to_owned())));
                    }
                    self.bytes.extend(data);
                }
                Err(rustix::io::Errno::AGAIN) => {}
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(io::Error::from(e)),
            }
        }
        Ok(self.parse())
    }
    fn starts(&self, prefix: &[u8]) -> bool {
        self.bytes.iter().zip(prefix).all(|(a, b)| a == b)
    }
    fn consume(&mut self, n: usize) {
        self.bytes.drain(..n);
    }
    fn parse(&mut self) -> Option<InputEvent> {
        let mut steps = 0;
        while !self.bytes.is_empty() && steps < 4096 {
            steps += 1;
            if self.mode == Mode::CsiDiscard {
                let b = self.bytes.pop_front().expect("nonempty");
                if b == 3 {
                    self.bytes.clear();
                    self.mode = Mode::Normal;
                    return Some(InputEvent::PasteCancelled);
                }
                if (0x40..=0x7e).contains(&b) {
                    self.mode = Mode::Normal;
                }
                continue;
            }
            if self.mode == Mode::Normal && self.after_esc {
                let rest = &START[1..];
                if self.starts(rest) {
                    if self.bytes.len() < rest.len() {
                        if self.eof {
                            self.bytes.clear();
                            self.after_esc = false;
                            return Some(InputEvent::Eof);
                        }
                        return None;
                    }
                    self.consume(rest.len());
                    self.after_esc = false;
                    self.mode = Mode::Discard;
                    self.cancelled = true;
                    return Some(InputEvent::PasteCancelled);
                }
                self.after_esc = false;
            }
            if self.mode != Mode::Normal {
                if self.bytes[0] == 3 {
                    self.consume(1);
                    self.paste.clear();
                    if !self.cancelled {
                        self.mode = Mode::Discard;
                        self.cancelled = true;
                        return Some(InputEvent::PasteCancelled);
                    }
                    return Some(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
                }
                if self.bytes[0] == 27 && self.starts(END) {
                    if self.bytes.len() < END.len() {
                        if self.eof {
                            self.bytes.clear();
                            self.paste.clear();
                            return Some(InputEvent::Eof);
                        }
                        return None;
                    }
                    self.consume(END.len());
                    let mode = self.mode;
                    self.mode = Mode::Normal;
                    self.cancelled = false;
                    if mode == Mode::Paste {
                        let data = String::from_utf8_lossy(&self.paste).into_owned();
                        self.paste.clear();
                        return Some(InputEvent::Terminal(Event::Paste(data)));
                    }
                    self.paste.clear();
                    continue;
                }
                let byte = self.bytes.pop_front().expect("nonempty");
                if self.mode == Mode::Paste {
                    if self.paste.len() == MAX {
                        self.paste.clear();
                        self.mode = Mode::Discard;
                        return Some(InputEvent::PasteOverflow);
                    }
                    self.paste.push(byte);
                }
                continue;
            }
            let first = self.bytes[0];
            if first == 27 {
                if self.starts(START) {
                    if self.bytes.len() < START.len() {
                        let since = self.esc_since.get_or_insert_with(Instant::now);
                        if self.eof
                            || (self.bytes.len() == 1
                                && since.elapsed() >= Duration::from_millis(30))
                        {
                            self.consume(1);
                            self.esc_since = None;
                            self.after_esc = true;
                            return Some(key(KeyCode::Esc, KeyModifiers::NONE));
                        }
                        return None;
                    }
                    self.consume(START.len());
                    self.esc_since = None;
                    self.mode = Mode::Paste;
                    self.cancelled = false;
                    return Some(InputEvent::PasteStarted);
                }
                if self.bytes.len() == 1 {
                    let since = self.esc_since.get_or_insert_with(Instant::now);
                    if since.elapsed() < Duration::from_millis(30) {
                        return None;
                    }
                    self.consume(1);
                    self.esc_since = None;
                    self.after_esc = true;
                    return Some(key(KeyCode::Esc, KeyModifiers::NONE));
                }
                if self.bytes[1] == b'[' {
                    let end = self
                        .bytes
                        .iter()
                        .enumerate()
                        .skip(2)
                        .take(32)
                        .find_map(|(i, b)| ((0x40..=0x7e).contains(b)).then_some(i));
                    if let Some(index) = self.bytes.iter().position(|b| *b == 3)
                        && end.is_none_or(|end| index < end)
                    {
                        self.consume(index + 1);
                        self.bytes.clear();
                        self.mode = Mode::Normal;
                        self.cancelled = false;
                        self.esc_since = None;
                        return Some(InputEvent::PasteCancelled);
                    }
                    let Some(end) = end else {
                        if self.eof {
                            self.bytes.clear();
                            return Some(InputEvent::Eof);
                        }
                        if self.bytes.len() > 34 {
                            self.consume(34);
                            self.mode = Mode::CsiDiscard;
                            continue;
                        }
                        return None;
                    };
                    let seq: Vec<u8> = self.bytes.iter().take(end + 1).copied().collect();
                    self.consume(end + 1);
                    self.esc_since = None;
                    let (code, mods) = match seq.as_slice() {
                        b"\x1b[A" => (KeyCode::Up, KeyModifiers::NONE),
                        b"\x1b[B" => (KeyCode::Down, KeyModifiers::NONE),
                        b"\x1b[C" => (KeyCode::Right, KeyModifiers::NONE),
                        b"\x1b[D" => (KeyCode::Left, KeyModifiers::NONE),
                        b"\x1b[Z" => (KeyCode::BackTab, KeyModifiers::SHIFT),
                        b"\x1b[3~" => (KeyCode::Delete, KeyModifiers::NONE),
                        b"\x1b[5~" => (KeyCode::PageUp, KeyModifiers::NONE),
                        b"\x1b[6~" => (KeyCode::PageDown, KeyModifiers::NONE),
                        b"\x1b[1;2A" => (KeyCode::Up, KeyModifiers::SHIFT),
                        b"\x1b[1;2B" => (KeyCode::Down, KeyModifiers::SHIFT),
                        _ => continue,
                    };
                    return Some(key(code, mods));
                }
                if self.bytes[1] == b'O' {
                    if self.bytes.len() < 3 {
                        if self.eof {
                            self.bytes.clear();
                            return Some(InputEvent::Eof);
                        }
                        return None;
                    }
                    let code = match self.bytes[2] {
                        b'A' => Some(KeyCode::Up),
                        b'B' => Some(KeyCode::Down),
                        b'C' => Some(KeyCode::Right),
                        b'D' => Some(KeyCode::Left),
                        _ => None,
                    };
                    self.consume(3);
                    self.esc_since = None;
                    if let Some(code) = code {
                        return Some(key(code, KeyModifiers::NONE));
                    }
                    continue;
                }
                let first = self.bytes[1];
                let len = if first < 128 {
                    1
                } else if first & 0xe0 == 0xc0 {
                    2
                } else if first & 0xf0 == 0xe0 {
                    3
                } else if first & 0xf8 == 0xf0 {
                    4
                } else {
                    self.consume(2);
                    continue;
                };
                if self.bytes.len() < len + 1 {
                    if self.eof {
                        self.bytes.clear();
                        return Some(InputEvent::Eof);
                    }
                    return None;
                }
                let bytes: Vec<u8> = self.bytes.iter().skip(1).take(len).copied().collect();
                self.consume(len + 1);
                self.esc_since = None;
                if let Ok(s) = std::str::from_utf8(&bytes)
                    && let Some(c) = s.chars().next()
                {
                    return Some(key(KeyCode::Char(c), KeyModifiers::ALT));
                }
                continue;
            }
            self.esc_since = None;
            let (code, mods, n) = match first {
                13 | 10 => (KeyCode::Enter, KeyModifiers::NONE, 1),
                9 => (KeyCode::Tab, KeyModifiers::NONE, 1),
                127 | 8 => (KeyCode::Backspace, KeyModifiers::NONE, 1),
                1..=26 => (
                    KeyCode::Char((b'a' + first - 1) as char),
                    KeyModifiers::CONTROL,
                    1,
                ),
                0..=31 => {
                    self.consume(1);
                    continue;
                }
                32..=126 => (KeyCode::Char(first as char), KeyModifiers::NONE, 1),
                _ => {
                    let n = if first & 0xe0 == 0xc0 {
                        2
                    } else if first & 0xf0 == 0xe0 {
                        3
                    } else if first & 0xf8 == 0xf0 {
                        4
                    } else {
                        self.consume(1);
                        continue;
                    };
                    if self.bytes.len() < n {
                        if self.eof {
                            self.bytes.clear();
                            return Some(InputEvent::Eof);
                        }
                        return None;
                    }
                    let data: Vec<u8> = self.bytes.iter().take(n).copied().collect();
                    let Ok(s) = std::str::from_utf8(&data) else {
                        self.consume(1);
                        continue;
                    };
                    (
                        KeyCode::Char(s.chars().next().expect("utf8")),
                        KeyModifiers::NONE,
                        n,
                    )
                }
            };
            self.consume(n);
            return Some(key(code, mods));
        }
        if self.eof && self.bytes.is_empty() {
            Some(InputEvent::Eof)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn feed(input: &mut TerminalInput, bytes: &[u8]) {
        input.bytes.extend(bytes);
    }
    #[test]
    fn split_utf8_and_escape_are_decoded() {
        let mut input = TerminalInput::default();
        feed(&mut input, &[0xe7, 0x94]);
        assert!(input.parse().is_none());
        feed(&mut input, &[0xb2]);
        assert!(
            matches!(input.parse(),Some(InputEvent::Terminal(Event::Key(e))) if e.code==KeyCode::Char('甲'))
        );
        feed(&mut input, b"\x1b[");
        assert!(input.parse().is_none());
        feed(&mut input, b"D");
        assert!(
            matches!(input.parse(),Some(InputEvent::Terminal(Event::Key(e))) if e.code==KeyCode::Left)
        );
    }
    #[test]
    fn late_marker_is_discarded_and_long_csi_recovers() {
        let mut input = TerminalInput::default();
        feed(&mut input, b"\x1b");
        assert!(input.parse().is_none());
        input.esc_since = Some(Instant::now() - Duration::from_secs(1));
        assert!(
            matches!(input.parse(),Some(InputEvent::Terminal(Event::Key(e))) if e.code==KeyCode::Esc)
        );
        feed(&mut input, b"[20");
        assert!(input.parse().is_none());
        feed(&mut input, b"0~alpha\r\nbeta\x1b[201~");
        assert!(matches!(input.parse(), Some(InputEvent::PasteCancelled)));
        assert!(input.parse().is_none());
        feed(&mut input, b"\x1b[");
        feed(&mut input, &[b'1'; 40]);
        feed(&mut input, b"~x");
        assert!(
            matches!(input.parse(),Some(InputEvent::Terminal(Event::Key(e))) if e.code==KeyCode::Char('x'))
        );
        assert_eq!(raw_batch(b"alpha\r\nbeta"), Some("alpha\r\nbeta"));
        assert_eq!(raw_batch(b"y\x03"), None);
    }
    #[test]
    fn slow_marker_ss3_and_alt_do_not_leak_commands() {
        let mut input = TerminalInput::default();
        feed(&mut input, b"\x1b[20");
        assert!(input.parse().is_none());
        input.esc_since = Some(Instant::now() - Duration::from_secs(1));
        assert!(input.parse().is_none());
        feed(&mut input, b"0~x\r\ny\x1b[201~");
        assert!(matches!(input.parse(), Some(InputEvent::PasteStarted)));
        assert!(matches!(input.parse(),Some(InputEvent::Terminal(Event::Paste(s))) if s=="x\r\ny"));
        feed(&mut input, b"\x1bOD");
        assert!(
            matches!(input.parse(),Some(InputEvent::Terminal(Event::Key(e))) if e.code==KeyCode::Left)
        );
        feed(&mut input, b"\x1bx");
        assert!(
            matches!(input.parse(),Some(InputEvent::Terminal(Event::Key(e))) if e.code==KeyCode::Char('x') && e.modifiers.contains(KeyModifiers::ALT))
        );
    }
    #[test]
    fn paste_is_atomic_and_preserves_crlf() {
        let mut input = TerminalInput::default();
        feed(&mut input, b"\x1b[200~alpha\r\nbeta\x1b[20");
        assert!(matches!(input.parse(), Some(InputEvent::PasteStarted)));
        assert!(input.parse().is_none());
        feed(&mut input, b"1~");
        assert!(
            matches!(input.parse(),Some(InputEvent::Terminal(Event::Paste(s))) if s=="alpha\r\nbeta")
        );
    }
    #[test]
    fn oversized_and_cancelled_paste_cannot_be_submitted() {
        let mut input = TerminalInput::default();
        feed(&mut input, START);
        feed(&mut input, &vec![b'x'; 65537]);
        let mut overflow = false;
        while !input.bytes.is_empty() {
            if matches!(input.parse(), Some(InputEvent::PasteOverflow)) {
                overflow = true;
            }
        }
        assert!(overflow);
        assert_eq!(input.paste.len(), 0);
        feed(&mut input, b"danger\r\x1b[201~");
        assert!(input.parse().is_none());
        feed(&mut input, b"\x1b[200~payload\x03");
        assert!(matches!(input.parse(), Some(InputEvent::PasteStarted)));
        assert!(matches!(input.parse(), Some(InputEvent::PasteCancelled)));
        feed(&mut input, b"\x03");
        assert!(
            matches!(input.parse(),Some(InputEvent::Terminal(Event::Key(e))) if e.code==KeyCode::Char('c') && e.modifiers.contains(KeyModifiers::CONTROL))
        );
    }
    #[test]
    fn eof_during_partial_sequences_is_bounded() {
        let mut input = TerminalInput::default();
        feed(&mut input, b"\x1b[200~a\x1b[20");
        input.eof = true;
        assert!(matches!(input.parse(), Some(InputEvent::PasteStarted)));
        assert!(matches!(input.parse(), Some(InputEvent::Eof)));
        let mut late = TerminalInput {
            after_esc: true,
            ..TerminalInput::default()
        };
        feed(&mut late, b"[20");
        late.eof = true;
        assert!(matches!(late.parse(), Some(InputEvent::Eof)));
    }
}
