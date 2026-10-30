//! JSON syntax is checked separately from JavaScript's value semantics.
//! All container traversal uses explicit stacks, including failure cleanup.
use crate::{CodecError, JsObject, JsString, JsValue};

struct Parser<'a> {
    raw: &'a str,
    at: usize,
}

enum ParseFrame {
    Array(Vec<JsValue>),
    Object(JsObject, JsString),
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while self
            .raw
            .as_bytes()
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.at += 1;
        }
    }

    fn error(&self, message: &str) -> CodecError {
        CodecError {
            offset: self.at,
            message: message.into(),
        }
    }

    fn string(&mut self) -> Result<JsString, CodecError> {
        if self.raw.as_bytes().get(self.at) != Some(&b'"') {
            return Err(self.error("Expected string"));
        }
        self.at += 1;
        let mut units = Vec::new();
        loop {
            match self.raw.as_bytes().get(self.at).copied() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(JsString::from_utf16(units));
                }
                Some(b'\\') => {
                    self.at += 1;
                    let escaped = match self.raw.as_bytes().get(self.at).copied() {
                        Some(b'"') => b'"' as u16,
                        Some(b'\\') => b'\\' as u16,
                        Some(b'/') => b'/' as u16,
                        Some(b'b') => 8,
                        Some(b'f') => 12,
                        Some(b'n') => 10,
                        Some(b'r') => 13,
                        Some(b't') => 9,
                        Some(b'u') => {
                            let mut code = 0u16;
                            for _ in 0..4 {
                                self.at += 1;
                                let digit = match self.raw.as_bytes().get(self.at).copied() {
                                    Some(b @ b'0'..=b'9') => b - b'0',
                                    Some(b @ b'a'..=b'f') => b - b'a' + 10,
                                    Some(b @ b'A'..=b'F') => b - b'A' + 10,
                                    _ => return Err(self.error("Invalid Unicode escape")),
                                };
                                code = (code << 4) | u16::from(digit);
                            }
                            code
                        }
                        _ => return Err(self.error("Invalid string escape")),
                    };
                    units.push(escaped);
                    self.at += 1;
                }
                Some(_) => {
                    let start = self.at;
                    while self
                        .raw
                        .as_bytes()
                        .get(self.at)
                        .is_some_and(|b| !matches!(b, b'"' | b'\\'))
                    {
                        self.at += 1;
                    }
                    // ASCII delimiters cannot split a valid UTF-8 character.
                    units.extend(self.raw[start..self.at].encode_utf16());
                }
                None => return Err(self.error("Unterminated string")),
            }
        }
    }

    fn object_key(&mut self) -> Result<JsString, CodecError> {
        self.whitespace();
        let key = self.string()?;
        self.whitespace();
        if self.raw.as_bytes().get(self.at) != Some(&b':') {
            return Err(self.error("Expected colon"));
        }
        self.at += 1;
        Ok(key)
    }
}

pub(super) fn parse(raw: &str) -> Result<JsValue, CodecError> {
    // RawValue's ignored-value scanner is iterative. It verifies JSON grammar
    // without constructing serde Value, normalizing strings, or parsing f64.
    serde_json::from_str::<&serde_json::value::RawValue>(raw).map_err(|error| {
        let line_start: usize = raw
            .split_inclusive('\n')
            .take(error.line().saturating_sub(1))
            .map(str::len)
            .sum();
        CodecError {
            offset: (line_start + error.column().saturating_sub(1)).min(raw.len()),
            message: error.to_string(),
        }
    })?;
    let mut parser = Parser { raw, at: 0 };
    let mut frames = Vec::new();
    loop {
        parser.whitespace();
        let mut value =
            match raw.as_bytes().get(parser.at).copied() {
                Some(b'[') => {
                    parser.at += 1;
                    parser.whitespace();
                    if raw.as_bytes().get(parser.at) == Some(&b']') {
                        parser.at += 1;
                        JsValue::Array(Vec::new())
                    } else {
                        frames.push(ParseFrame::Array(Vec::new()));
                        continue;
                    }
                }
                Some(b'{') => {
                    parser.at += 1;
                    parser.whitespace();
                    if raw.as_bytes().get(parser.at) == Some(&b'}') {
                        parser.at += 1;
                        JsValue::Object(JsObject::new())
                    } else {
                        let key = parser.object_key()?;
                        frames.push(ParseFrame::Object(JsObject::new(), key));
                        continue;
                    }
                }
                Some(b'"') => JsValue::String(parser.string()?),
                Some(b'n') => {
                    parser.at += 4;
                    JsValue::Null
                }
                Some(b't') => {
                    parser.at += 4;
                    JsValue::Bool(true)
                }
                Some(b'f') => {
                    parser.at += 5;
                    JsValue::Bool(false)
                }
                Some(b'-' | b'0'..=b'9') => {
                    let start = parser.at;
                    while raw.as_bytes().get(parser.at).is_some_and(|b| {
                        matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                    }) {
                        parser.at += 1;
                    }
                    let number = raw[start..parser.at]
                        .parse::<f64>()
                        .map_err(|_| parser.error("Cannot parse JSON number as binary64"))?;
                    JsValue::Number(number)
                }
                _ => return Err(parser.error("Expected value")),
            };
        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(value);
            };
            match frame {
                ParseFrame::Array(items) => items.push(value),
                ParseFrame::Object(object, key) => {
                    object.insert(std::mem::take(key), value);
                }
            }
            parser.whitespace();
            if raw.as_bytes().get(parser.at) == Some(&b',') {
                parser.at += 1;
                if let ParseFrame::Object(_, key) = frame {
                    *key = parser.object_key()?;
                }
                break;
            }
            let expected = match frame {
                ParseFrame::Array(_) => b']',
                ParseFrame::Object(_, _) => b'}',
            };
            if raw.as_bytes().get(parser.at) != Some(&expected) {
                return Err(parser.error("Expected container delimiter"));
            }
            parser.at += 1;
            value = match frames.pop() {
                Some(ParseFrame::Array(items)) => JsValue::Array(items),
                Some(ParseFrame::Object(object, _)) => JsValue::Object(object),
                None => return Err(parser.error("Missing container frame")),
            };
        }
    }
}

fn unicode_escape(unit: u16, output: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push_str("\\u");
    for shift in [12, 8, 4, 0] {
        output.push(char::from(HEX[usize::from((unit >> shift) & 15)]));
    }
}

pub(crate) fn quoted(value: &JsString, output: &mut String) {
    output.push('"');
    let mut units = value.units().iter().copied().peekable();
    while let Some(unit) = units.next() {
        match unit {
            8 => output.push_str("\\b"),
            9 => output.push_str("\\t"),
            10 => output.push_str("\\n"),
            12 => output.push_str("\\f"),
            13 => output.push_str("\\r"),
            0..=31 => unicode_escape(unit, output),
            34 => output.push_str("\\\""),
            92 => output.push_str("\\\\"),
            0xd800..=0xdbff => {
                if let Some(low @ 0xdc00..=0xdfff) = units.peek().copied() {
                    units.next();
                    let scalar =
                        0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
                    if let Some(character) = char::from_u32(scalar) {
                        output.push(character);
                    }
                } else {
                    unicode_escape(unit, output);
                }
            }
            0xdc00..=0xdfff => unicode_escape(unit, output),
            _ => {
                if let Some(character) = char::from_u32(u32::from(unit)) {
                    output.push(character);
                }
            }
        }
    }
    output.push('"');
}

enum Emit<'a> {
    Value(&'a JsValue),
    /// An array resumed at `index`: frames carry one element at a time instead
    /// of stacking the whole breadth, so a wide projection serializes with
    /// scratch bounded by depth (#983).
    Array(&'a [JsValue], usize),
    String(&'a JsString),
    Character(char),
}

pub(super) fn stringify(value: &JsValue) -> Result<Option<String>, CodecError> {
    if value.is_undefined() {
        return Ok(None);
    }
    let mut output = String::new();
    let mut pending = vec![Emit::Value(value)];
    let mut number_buffer = ryu_js::Buffer::new();
    while let Some(task) = pending.pop() {
        match task {
            Emit::Character(character) => output.push(character),
            Emit::String(string) => quoted(string, &mut output),
            Emit::Array(items, index) => {
                if index == items.len() {
                    output.push(']');
                } else {
                    if index > 0 {
                        output.push(',');
                    }
                    pending.push(Emit::Array(items, index + 1));
                    pending.push(Emit::Value(&items[index]));
                }
            }
            Emit::Value(value) => match value {
                JsValue::Undefined | JsValue::Null => output.push_str("null"),
                JsValue::Bool(true) => output.push_str("true"),
                JsValue::Bool(false) => output.push_str("false"),
                JsValue::Number(number) => {
                    if !number.is_finite() {
                        output.push_str("null");
                    } else if *number == 0.0 {
                        output.push('0');
                    } else {
                        output.push_str(number_buffer.format_finite(*number));
                    }
                }
                JsValue::String(string) => quoted(string, &mut output),
                JsValue::Array(items) => {
                    output.push('[');
                    pending.push(Emit::Array(items, 0));
                }
                JsValue::Object(object) => {
                    let ordered = object
                        .own_entries()
                        .into_iter()
                        .filter(|(_, item)| !item.is_undefined())
                        .collect::<Vec<_>>();
                    output.push('{');
                    pending.push(Emit::Character('}'));
                    for (index, (key, item)) in ordered.into_iter().enumerate().rev() {
                        pending.push(Emit::Value(item));
                        pending.push(Emit::Character(':'));
                        pending.push(Emit::String(key));
                        if index > 0 {
                            pending.push(Emit::Character(','));
                        }
                    }
                }
            },
        }
    }
    Ok(Some(output))
}

enum CloneTask<'a> {
    Visit(&'a JsValue),
    Array(usize),
    Object(&'a JsObject),
}

pub(super) fn clone_value(value: &JsValue) -> JsValue {
    let mut tasks = vec![CloneTask::Visit(value)];
    let mut values = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            CloneTask::Visit(value) => match value {
                JsValue::Undefined => values.push(JsValue::Undefined),
                JsValue::Null => values.push(JsValue::Null),
                JsValue::Bool(value) => values.push(JsValue::Bool(*value)),
                JsValue::Number(value) => values.push(JsValue::Number(*value)),
                JsValue::String(value) => values.push(JsValue::String(value.clone())),
                JsValue::Array(items) => {
                    tasks.push(CloneTask::Array(items.len()));
                    tasks.extend(items.iter().rev().map(CloneTask::Visit));
                }
                JsValue::Object(object) => {
                    tasks.push(CloneTask::Object(object));
                    if let Some(prototype) = object.prototype() {
                        tasks.push(CloneTask::Visit(prototype));
                    }
                    tasks.extend(
                        object
                            .entries()
                            .iter()
                            .rev()
                            .map(|(_, value)| CloneTask::Visit(value)),
                    );
                }
            },
            CloneTask::Array(length) => {
                let items = values.split_off(values.len() - length);
                values.push(JsValue::Array(items));
            }
            CloneTask::Object(source) => {
                let mut children = values.split_off(
                    values.len() - source.len() - usize::from(source.prototype().is_some()),
                );
                let mut object = JsObject::new();
                if source.prototype().is_some() {
                    object.set_prototype(children.pop().expect("visited prototype"));
                }
                if let Some(tag) = source.native_tag() {
                    object.set_native_tag(tag);
                }
                for ((key, _), child) in source.entries().iter().zip(children) {
                    object.insert(key.clone(), child);
                }
                values.push(JsValue::Object(object));
            }
        }
    }
    // Every Visit produces exactly one value after its children are assembled.
    assert_eq!(values.len(), 1, "clone traversal assembles one root");
    values.swap_remove(0)
}
