// Port of yaml@2.9.0 dist/compose/resolve-end.js (ISC).
use super::Context;
use crate::{
    parse::cst::{Token, TokenType::*},
    string::{append, slice},
};
use pilotdeck_protocol::JsString;
pub(super) struct End {
    pub comment: JsString,
    pub offset: usize,
}
pub(super) fn resolve_end(
    ctx: &mut Context,
    end: &[Token],
    mut offset: usize,
    req_space: bool,
) -> End {
    let mut comment = JsString::default();
    let mut has_space = false;
    let mut sep = JsString::default();
    for token in end {
        let source = token.source.as_ref().unwrap();
        match token.kind {
            Space => has_space = true,
            Comment => {
                if req_space && !has_space {
                    ctx.token_error(
                        token,
                        "MISSING_CHAR",
                        "Comments must be separated from other tokens by white space characters",
                    );
                }
                let cb = if source.len_utf16() > 1 {
                    slice(source, 1, source.len_utf16())
                } else {
                    " ".into()
                };
                if comment.is_empty() {
                    comment = cb;
                } else {
                    append(&mut comment, &sep);
                    append(&mut comment, &cb);
                }
                sep = JsString::default();
            }
            Newline => {
                if !comment.is_empty() {
                    append(&mut sep, source);
                }
                has_space = true;
            }
            _ => ctx.token_error(
                token,
                "UNEXPECTED_TOKEN",
                format!("Unexpected {} at node end", token.kind.as_str()),
            ),
        }
        offset += source.len_utf16();
    }
    End { comment, offset }
}
