// Port of yaml@2.9.0 dist/compose/util-contains-newline.js (ISC).
use crate::parse::cst::{Token, TokenType::*};
pub(super) fn contains_newline(key: Option<&Token>) -> bool {
    let Some(key) = key else { return false };
    match key.kind {
        Alias | Scalar | DoubleQuotedScalar | SingleQuotedScalar => {
            key.source.as_ref().unwrap().units().contains(&10)
                || key
                    .end
                    .as_ref()
                    .is_some_and(|end| end.iter().any(|t| t.kind == Newline))
        }
        FlowCollection => key.items.iter().any(|it| {
            it.start.iter().any(|t| t.kind == Newline)
                || it
                    .sep
                    .as_ref()
                    .is_some_and(|s| s.iter().any(|t| t.kind == Newline))
                || contains_newline(it.key.as_ref().and_then(|k| k.as_deref()))
                || contains_newline(it.value.as_deref())
        }),
        _ => true,
    }
}
