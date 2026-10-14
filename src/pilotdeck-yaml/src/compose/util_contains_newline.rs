// Port of yaml@2.9.0 dist/compose/util-contains-newline.js (ISC).
use crate::parse::cst::{Token, TokenType::*};
pub(super) fn contains_newline(key: Option<&Token>) -> bool {
    let mut pending: Vec<_> = key.into_iter().collect();
    while let Some(key) = pending.pop() {
        match key.kind {
            Alias | Scalar | DoubleQuotedScalar | SingleQuotedScalar => {
                if key.source.as_ref().unwrap().units().contains(&10)
                    || key
                        .end
                        .as_ref()
                        .is_some_and(|end| end.iter().any(|t| t.kind == Newline))
                {
                    return true;
                }
            }
            FlowCollection => {
                for it in &key.items {
                    if it.start.iter().any(|t| t.kind == Newline)
                        || it
                            .sep
                            .as_ref()
                            .is_some_and(|s| s.iter().any(|t| t.kind == Newline))
                    {
                        return true;
                    }
                    if let Some(Some(key)) = &it.key {
                        pending.push(key);
                    }
                    if let Some(value) = it.value.as_deref() {
                        pending.push(value);
                    }
                }
            }
            _ => return true,
        }
    }
    false
}
