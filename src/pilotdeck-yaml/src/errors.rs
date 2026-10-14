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
        token.offset + token.source.as_ref().map_or(1, JsString::len_utf16),
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
    let mut line_str = slice(src, start, end);
    let trimmed = line_str
        .units()
        .iter()
        .rposition(|c| !matches!(c, 10 | 13))
        .map_or(0, |i| i + 1);
    line_str = slice(&line_str, 0, trimmed);
    if ci >= 60 && line_str.len_utf16() > 80 {
        let trim_start = (ci - 39).min(line_str.len_utf16() - 79);
        line_str = crate::string::concat(&[
            &"…".into(),
            &slice(&line_str, trim_start, line_str.len_utf16()),
        ]);
        ci -= trim_start - 1;
    }
    if line_str.len_utf16() > 80 {
        line_str = crate::string::concat(&[&slice(&line_str, 0, 79), &"…".into()]);
    }
    if line > 1
        && line_str.units()[..ci.min(line_str.len_utf16())]
            .iter()
            .all(|c| *c == 32)
    {
        let mut prev = slice(src, lc.line_starts[line - 2], lc.line_starts[line - 1]);
        if prev.len_utf16() > 80 {
            prev = crate::string::concat(&[&slice(&prev, 0, 79), &"…\n".into()]);
        }
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
