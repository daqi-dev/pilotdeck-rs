// Port of yaml@2.9.0 dist/compose/resolve-end.js (ISC).
use super::Context;
use crate::parse::cst::{Token, TokenType::*};
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
    let mut comment = Vec::new();
    let mut has_space = false;
    let mut sep = Vec::new();
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
                if !comment.is_empty() {
                    comment.extend_from_slice(&sep);
                }
                if source.len_utf16() > 1 {
                    comment.extend_from_slice(&source.units()[1..]);
                } else {
                    comment.push(32);
                }
                sep.clear();
            }
            Newline => {
                if !comment.is_empty() {
                    sep.extend_from_slice(source.units());
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
    End {
        comment: JsString::from_utf16(comment),
        offset,
    }
}
