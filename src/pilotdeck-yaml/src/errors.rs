// Port of yaml@2.9.0 dist/errors.js (ISC).
use crate::{
    parse::{
        cst::Token,
        line_counter::{LineCounter, LinePos},
    },
    string::{append, slice},
};
use pilotdeck_protocol::JsString;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub name: &'static str,
    pub code: &'static str,
    pub message: JsString,
    pub pos: [usize; 2],
    pub line_pos: Option<[LinePos; 2]>,
}
impl Diagnostic {
    pub(crate) fn new(
        pos: [usize; 2],
        code: &'static str,
        message: impl Into<JsString>,
        warning: bool,
    ) -> Self {
        Self {
            name: if warning {
                "YAMLWarning"
            } else {
                "YAMLParseError"
            },
            code,
            message: message.into(),
            pos,
            line_pos: None,
        }
    }
}
pub(crate) fn token_pos(token: &Token) -> [usize; 2] {
    [
        token.offset,
        token.offset + token.source.as_ref().map_or(1, |source| source.len_utf16()),
    ]
}
pub(crate) fn prettify_error(src: &JsString, lc: &LineCounter, error: &mut Diagnostic) {
    let lp = error.pos.map(|p| lc.line_pos(p));
    error.line_pos = Some(lp);
    let LinePos { line, col } = lp[0];
    append(
        &mut error.message,
        &format!(" at line {line}, column {col}").into(),
    );
    let mut ci = col.saturating_sub(1);
    let start = lc
        .line_starts
        .get(line.wrapping_sub(1))
        .copied()
        .unwrap_or(0);
    let end = lc.line_starts.get(line).copied().unwrap_or(src.len_utf16());
    // Diagnostics display at most 80 units. Slice the borrowed source first;
    // copying a whole long line for every error made this loop quadratic.
    let raw = &src.units()
        [start.min(src.len_utf16())..end.min(src.len_utf16()).max(start.min(src.len_utf16()))];
    let trimmed = raw
        .iter()
        .rposition(|c| !matches!(c, 10 | 13))
        .map_or(0, |i| i + 1);
    let mut visible = &raw[..trimmed];
    let mut display = Vec::new();
    if ci >= 60 && visible.len() > 80 {
        let trim_start = (ci - 39).min(visible.len() - 79);
        visible = &visible[trim_start..];
        display.push(0x2026);
        ci -= trim_start - 1;
    }
    let truncated = display.len() + visible.len() > 80;
    let count = if truncated {
        79 - display.len()
    } else {
        visible.len()
    };
    display.extend_from_slice(&visible[..count]);
    if truncated {
        display.push(0x2026);
    }
    let mut line_str = JsString::from_utf16(display);
    if line > 1
        && line_str.units()[..ci.min(line_str.len_utf16())]
            .iter()
            .all(|c| *c == 32)
    {
        let prev_start = lc.line_starts[line - 2];
        let prev_end = lc.line_starts[line - 1];
        let prev = if prev_end - prev_start > 80 {
            crate::string::concat(&[&slice(src, prev_start, prev_start + 79), &"…\n".into()])
        } else {
            slice(src, prev_start, prev_end)
        };
        line_str = crate::string::concat(&[&prev, &line_str]);
    }
    if line_str.units().iter().any(|c| *c != 32) {
        let end = lp[1];
        let count = if end.line == line && end.col > col {
            (end.col - col).min(80usize.saturating_sub(ci)).max(1)
        } else {
            1
        };
        append(
            &mut error.message,
            &crate::string::concat(&[
                &":\n\n".into(),
                &line_str,
                &format!("\n{}{}\n", " ".repeat(ci), "^".repeat(count)).into(),
            ]),
        );
    }
}
